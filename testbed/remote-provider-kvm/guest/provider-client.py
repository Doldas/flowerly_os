#!/usr/bin/env python3
"""Provider socket client for the Arch/KVM acceptance guest (#263). TEST ONLY.

    provider-client.py admission <socket> [--wait-file F]
    provider-client.py discover  <socket> <model>
    provider-client.py submit    <socket> <model> <tag> [--request-id ID] [--variant N] [--no-wait]
    provider-client.py poll|cancel <socket> <request-id>

Runs as whatever identity the guest runner gives it (runuser/setpriv), so
the provider sees that process's kernel credentials. Prompts are synthetic
and carry the FLOWERLY-SENTINEL-PROMPT marker so a leak is detectable.
Output is one JSON object of metadata: never the prompt, never model text.

`admission` asks for a request id nobody submitted. A reply of class
`unknown_request` proves the provider's handler ran, so the peer was
admitted; a refused connect, a closed connection or any other reply means
it was not.
"""
import argparse
import json
import os
import re
import socket
import struct
import sys
import time
import uuid

POLL_SECONDS = 150
SENTINEL = "FLOWERLY-SENTINEL-PROMPT"
DETAIL = re.compile(r"\[(?P<cls>[a-z_]+); outcome (?P<outcome>[a-z_]+)\]\s*$")


def recv_exact(conn, count):
    data = bytearray()
    while len(data) < count:
        chunk = conn.recv(count - len(data))
        if not chunk:
            raise ConnectionError("closed")
        data.extend(chunk)
    return bytes(data)


def request(path, name, args):
    message = json.dumps({"version": 1, "id": str(uuid.uuid4()), "type": "command",
                          "name": name, "args": args}).encode()
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(60)
        client.connect(path)
        client.sendall(struct.pack("<I", len(message)) + message)
        return json.loads(recv_exact(client, struct.unpack("<I", recv_exact(client, 4))[0]))


def failure(reply):
    """class/outcome of an error reply (from its message) or a failed job."""
    if reply.get("type") == "ok":
        error = (reply.get("data") or {}).get("error")
        if isinstance(error, dict):
            return {"class": error.get("class"), "outcome": error.get("outcome"),
                    "retry_after_ms": error.get("retry_after_ms")}
        return None
    match = DETAIL.search(str(reply.get("message", "")))
    return {"code": reply.get("code"), "class": match["cls"] if match else None,
            "outcome": match["outcome"] if match else None}


def admission(path, wait_file):
    if wait_file:
        deadline = time.monotonic() + 120
        while not os.path.exists(wait_file):
            if time.monotonic() > deadline:
                return {"admitted": False, "error": "wait_file_timeout"}
            time.sleep(0.1)
    identity = {key: value for key, value in
                (line.split(":", 1) for line in open("/proc/self/status") if ":" in line)
                if key in ("Uid", "Gid", "Groups")}
    identity = {key: value.split() for key, value in identity.items()}
    client = socket.socket(socket.AF_UNIX)
    client.settimeout(30)
    try:
        client.connect(path)
    except OSError as err:
        client.close()
        # Refused before the provider saw us (the socket's file mode).
        return {"admitted": False, "connect_error": os.strerror(err.errno) if err.errno else type(err).__name__,
                "identity": identity}
    message = json.dumps({"version": 1, "id": str(uuid.uuid4()), "type": "command", "name": "provider.poll",
                          "args": {"request_id": f"admission-{uuid.uuid4()}"}}).encode()
    try:
        with client:
            client.sendall(struct.pack("<I", len(message)) + message)
            reply = json.loads(recv_exact(client, struct.unpack("<I", recv_exact(client, 4))[0]))
    except (OSError, ValueError) as err:
        # Connected, then refused by the provider's peer check.
        return {"admitted": False, "refused_after_connect": type(err).__name__, "identity": identity}
    detail = failure(reply) or {}
    return {"admitted": detail.get("class") == "unknown_request", "reply": detail, "identity": identity}


def discover(path, model):
    reply = request(path, "provider.discover", {})
    if reply.get("type") != "ok":
        return {"ok": False, "error": failure(reply)}
    data = reply["data"]
    return {"ok": True, "model_served": model in (data.get("models_available") or []),
            "discover_only": data.get("discover_only"), "destination": data.get("destination")}


def summarize(status):
    data = status.get("data") or {}
    out = {"state": data.get("state"), "sent": data.get("sent")}
    if data.get("state") == "succeeded":
        result = data.get("result") or {}
        out.update(model_used=result.get("model_used"), usage=result.get("usage"),
                   latency_ms=result.get("latency_ms"), finish_reason=result.get("finish_reason"),
                   text_present="text" in result, text_length=len(result.get("text") or ""))
    else:
        out["error"] = failure(status)
    return out


def submit(path, model, tag, request_id, variant, wait):
    request_id = request_id or f"kvm-{tag}-{uuid.uuid4().hex[:12]}"
    prompt = f"{SENTINEL}-{tag}-{variant}: reply with one word."
    started = time.monotonic()
    reply = request(path, "provider.submit", {"request_id": request_id, "prompt": prompt,
                                              "model": model, "max_tokens": 16})
    out = {"request_id": request_id, "submit": {"type": reply.get("type")}}
    if reply.get("type") != "ok":
        out["submit"]["error"] = failure(reply)
        return out
    if not wait:
        out["final"] = summarize(reply)
        return out
    status = reply
    while time.monotonic() < started + POLL_SECONDS:
        status = request(path, "provider.poll", {"request_id": request_id})
        if status.get("type") != "ok" or status["data"]["state"] != "pending":
            break
        time.sleep(0.25)
    out["final"] = summarize(status) if status.get("type") == "ok" else {"poll_error": failure(status)}
    return out


def main(argv):
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="mode", required=True)
    a = sub.add_parser("admission")
    a.add_argument("socket")
    a.add_argument("--wait-file")
    d = sub.add_parser("discover")
    d.add_argument("socket")
    d.add_argument("model")
    s = sub.add_parser("submit")
    s.add_argument("socket")
    s.add_argument("model")
    s.add_argument("tag")
    s.add_argument("--request-id")
    s.add_argument("--variant", type=int, default=0)
    s.add_argument("--no-wait", action="store_true")
    for mode in ("poll", "cancel"):
        p = sub.add_parser(mode)
        p.add_argument("socket")
        p.add_argument("request_id")
    args = parser.parse_args(argv[1:])
    try:
        if args.mode == "admission":
            out = admission(args.socket, args.wait_file)
        elif args.mode == "discover":
            out = discover(args.socket, args.model)
        elif args.mode == "submit":
            out = submit(args.socket, args.model, args.tag, args.request_id, args.variant, not args.no_wait)
        else:
            reply = request(args.socket, f"provider.{args.mode}", {"request_id": args.request_id})
            out = {"type": reply.get("type"), "error": failure(reply),
                   "state": (reply.get("data") or {}).get("state")}
    except (OSError, ValueError) as err:
        out = {"client_error": type(err).__name__}
    print(json.dumps(out, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
