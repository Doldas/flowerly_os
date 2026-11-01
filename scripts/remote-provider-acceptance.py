#!/usr/bin/env python3
"""Operator-run acceptance client for one remote provider instance (#263).

NOT run by CI against a real server. `scripts/remote-provider-operator.sh`
runs it as the provider's service account (the provider socket admits that
account and its client group, never root). Modes:

- `dns <host>`: resolve the destination and require every address to be
  public (ADR-0022 Q2). No connection is made to the destination.
- `discover <socket>`: model discovery only. Sends no prompt.
- `accept <socket>`: exactly one fixed prompt, and only after discovery
  confirms the model is served. Run only with the owner's explicit approval.
- `evidence`: the operator script's evidence writer (inputs in the
  environment). The journal is scanned for leaks and never copied.

Output is one JSON object of metadata. The generated text is compared with
the expected reply and its length reported; it is never printed or stored.
"""
import ipaddress
import json
import socket
import struct
import sys
import time
import uuid

PROMPT = "Reply with exactly: flowerly-ready"
EXPECTED = "flowerly-ready"
MAX_TOKENS = 16
POLL_SECONDS = 150
ACTOR = "uid:acceptance"


NAT64 = ipaddress.ip_network("64:ff9b::/32")
IPV4_COMPATIBLE = ipaddress.ip_network("::/96")


def classify(address):
    """`public` only for globally routable unicast addresses, refusing at
    least what providerd's address policy refuses (net.rs)."""
    ip = ipaddress.ip_address(address)
    if ip.version == 6 and ip.ipv4_mapped:
        ip = ip.ipv4_mapped
    if ip.version == 6 and (ip in NAT64 or ip in IPV4_COMPATIBLE):
        return "refused"
    if ip.is_global and not ip.is_multicast:
        return "public"
    return "refused"


def dns(host):
    infos = socket.getaddrinfo(host, 443, type=socket.SOCK_STREAM)
    addresses = sorted({info[4][0] for info in infos})
    verdicts = {address: classify(address) for address in addresses}
    ok = bool(addresses) and all(v == "public" for v in verdicts.values())
    return ok, {"step": "dns", "host": host, "addresses": verdicts, "all_public": ok}


def recv_exact(conn, count):
    data = bytearray()
    while len(data) < count:
        chunk = conn.recv(count - len(data))
        if not chunk:
            raise ConnectionError("IPC connection closed during response")
        data.extend(chunk)
    return bytes(data)


def request(sock, name, args):
    message = json.dumps({"version": 1, "id": str(uuid.uuid4()), "type": "command",
                          "name": name, "args": args}).encode()
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(60)
        client.connect(sock)
        client.sendall(struct.pack("<I", len(message)) + message)
        return json.loads(recv_exact(client, struct.unpack("<I", recv_exact(client, 4))[0]))


def describe(described):
    return {key: described.get(key) for key in (
        "destination", "api_style", "models_allowlisted", "models_available",
        "discover_only", "max_tokens_per_request", "request_timeout_ms")}


def discover(sock, model):
    reply = request(sock, "provider.discover", {})
    if reply.get("type") != "ok":
        return False, {"step": "discover", "prompt_sent": False, "error": error_fields(reply)}
    described = reply["data"]
    served = model in (described.get("models_available") or [])
    report = {"step": "discover", "prompt_sent": False, "model": model,
              "model_served": served, "provider": describe(described)}
    return served, report


def error_fields(reply):
    return {"code": reply.get("code"), "message": reply.get("message")}


def accept(sock, model):
    served, report = discover(sock, model)
    if not served:
        report["reason"] = "the model is not served; no prompt was sent"
        return False, report
    if report["provider"].get("discover_only"):
        report["reason"] = "the instance is in discovery-only mode; no prompt was sent"
        return False, report

    request_id = f"acceptance-{uuid.uuid4()}"
    started = time.monotonic()
    submitted = request(sock, "provider.submit", {
        "request_id": request_id, "prompt": PROMPT, "model": model,
        "max_tokens": MAX_TOKENS, "created_by": ACTOR,
    })
    base = {"step": "accept", "request_id": request_id, "model": model,
            "max_tokens": MAX_TOKENS, "provider": report["provider"]}
    if submitted.get("type") != "ok":
        return False, dict(base, state="submit_refused", error=error_fields(submitted))
    status = submitted
    while time.monotonic() < started + POLL_SECONDS:
        status = request(sock, "provider.poll", {"request_id": request_id, "created_by": ACTOR})
        if status.get("type") != "ok" or status["data"]["state"] != "pending":
            break
        time.sleep(0.5)
    if status.get("type") != "ok":
        return False, dict(base, state="poll_failed", error=error_fields(status))
    data = status["data"]
    if data["state"] == "pending":
        # Not cancelled: the server may already be executing it.
        return False, dict(base, state="pending_after_poll_window", sent=data.get("sent"))
    if data["state"] != "succeeded":
        return False, dict(base, state=data["state"], error=data.get("error"))

    result = dict(data["result"])
    text = result.pop("text", None)
    again = request(sock, "provider.poll", {"request_id": request_id, "created_by": ACTOR})
    released = (again.get("data") or {}).get("result") or {}
    matches = (text or "").strip() == EXPECTED
    return matches, dict(
        base, state="succeeded", destination=result.get("destination"),
        model_used=result.get("model_used"), finish_reason=result.get("finish_reason"),
        truncated=result.get("truncated"), usage=result.get("usage"),
        # The server may not honor max_tokens (16 requested, 44 reported in
        # the first live test): recorded, never treated as a budget.
        max_tokens_requested=result.get("max_tokens_requested"),
        max_tokens_exceeded=result.get("max_tokens_exceeded"),
        latency_ms=result.get("latency_ms"), text_present=text is not None,
        text_length=len(text or ""), text_matches_expected=matches,
        text_released_after_delivery=released.get("text_released") is True and "text" not in released)


LEAK_MARKERS = {
    "contains_prompt": lambda text: PROMPT in text,
    "contains_bearer": lambda text: "bearer " in text.lower(),
    "contains_sk_token": lambda text: "sk-" in text,
}


def scan(text):
    return {name: test(text) for name, test in LEAK_MARKERS.items()}


def evidence(env):
    with open(env["JOURNAL"], errors="replace") as journal:
        journal_text = journal.read()
    with open(env["REPORTS"]) as reports:
        steps = [json.loads(line) for line in reports if line.strip()]
    found = scan(journal_text)
    record = {
        "issue": "#263",
        "step": env["STEP"],
        "instance": env["INSTANCE"],
        # What the operator typed. Nothing verifies it: it attests that the
        # operator holds an approval, it does not prove the owner gave one.
        "approval_reference": env.get("APPROVAL") or None,
        "approval_reference_semantics": "operator attestation; not verified; not cryptographic proof of owner authorization",
        "recorded_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "git_commit": env["GIT_COMMIT"],
        "git_tree_clean": env["GIT_CLEAN"] == "yes",
        "systemd": env["SYSTEMD_VERSION"],
        "unit_exposure": env.get("EXPOSURE", ""),
        "identity": {
            "client_group_members": env.get("CLIENT_GROUP_MEMBERS", ""),
            "provider_supplementary_groups": env.get("PROVIDER_GROUPS", ""),
        },
        "sha256": {key: env[f"SHA_{key.upper()}"] for key in
                   ("providerd", "client", "unit", "dropin", "config")},
        "steps": steps,
        "journal_scan": dict(found, lines=len(journal_text.splitlines())),
    }
    # The steps carry metadata only; refuse to write anything that is not.
    serialized = json.dumps(record, indent=2, sort_keys=True)
    leaks = [name for name, hit in found.items() if hit]
    leaks += [f"evidence_{name}" for name, hit in scan(serialized.replace(PROMPT, "")).items() if hit]
    if any(step.get("text") is not None for step in steps):
        leaks.append("evidence_contains_text")
    if not leaks:
        with open(env["EVIDENCE_PATH"], "x") as out:
            out.write(serialized + "\n")
    print(json.dumps({"evidence": None if leaks else env["EVIDENCE_PATH"], "leaks": leaks}, indent=2))
    return 1 if leaks else 0


def main(argv):
    if len(argv) == 2 and argv[1] == "evidence":
        import os
        return evidence(os.environ)
    if len(argv) not in (3, 4) or argv[1] not in ("dns", "discover", "accept"):
        sys.exit(f"usage: {argv[0]} dns <host> | discover <socket> [model] | accept <socket> [model]")
    mode, target = argv[1], argv[2]
    model = argv[3] if len(argv) == 4 else "gpt-oss:20b"
    try:
        if mode == "dns":
            ok, report = dns(target)
        elif mode == "discover":
            ok, report = discover(target, model)
        else:
            ok, report = accept(target, model)
    except (OSError, ValueError) as err:
        ok, report = False, {"step": mode, "error": {"class": type(err).__name__, "message": str(err)}}
    report["ok"] = ok
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
