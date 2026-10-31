#!/usr/bin/env python3
"""Offline mock of an OpenAI-compatible inference server, its DNS and the
destinations providerd must never reach (Arch/KVM acceptance, #263).

TEST ONLY. It runs inside the disposable guest (or on loopback in the
offline harness tests) and talks to nothing outside it. It holds no real
key, no real destination and no real prompt.

Three parts, all driven by one scenario word the guest runner writes to
<state>/scenario (re-read on every query and connection):

- an HTTPS server for the `mock` role that answers /v1 and /api model
  discovery and chat completions as the scenario says (success, auth
  failure, 429, 5xx, redirect, oversized, malformed, tool calls, missing
  usage, dropped body, delay, TLS from an untrusted CA or for a wrong name);
- a DNS server for *.flowerly.test whose answers the scenario chooses
  (public, private, CGNAT, loopback, link-local, metadata, ULA, or a
  deterministic rebinding sequence), always with TTL 0;
- plain TCP sinks on every other address, which only record that
  something connected. A refused destination must leave its sink empty.

Logged to <state>/events.jsonl: metadata only (role, path, status, sizes,
whether the bearer token matched the dummy key's SHA-256). Never the
prompt, the reply text, the token or a header value.
"""
import argparse
import hashlib
import json
import os
import pathlib
import socket
import ssl
import struct
import sys
import threading
import time

MAX_HEADER = 64 * 1024
MAX_BODY = 1024 * 1024
OVERSIZED = 1024 * 1024 + 1
DELAY_SECONDS = 5
RETRY_AFTER_SECONDS = 7
REPLY_TEXT = "flowerly-ready"
SENTINEL_REPLY = "FLOWERLY-SENTINEL-REPLY"
SENTINEL_PROMPT = "FLOWERLY-SENTINEL-PROMPT"

HTTP_SCENARIOS = {
    "ok", "missing-model", "model-mismatch", "auth-failure", "rate-limited", "server-error",
    "redirect", "oversized", "malformed-json", "tool-calls", "missing-usage", "unknown-outcome",
    "delayed", "empty-output", "tls-untrusted", "tls-wrong-name",
}
# Scenario -> address role the mock name resolves to (A or AAAA).
ADDRESS_SCENARIOS = {
    "addr-private": "private_rfc1918", "addr-cgnat": "cgnat", "addr-loopback": "loopback_v4",
    "addr-link-local": "link_local_v4", "addr-metadata": "metadata_v4", "addr-ula": "ula",
}
SCENARIOS = HTTP_SCENARIOS | set(ADDRESS_SCENARIOS) | {"rebind"}


class State:
    def __init__(self, directory, network):
        self.dir = pathlib.Path(directory)
        self.network = network
        self.lock = threading.Lock()
        self.rebind_line = None
        self.rebind_answers = 0

    def scenario(self):
        """(word, raw line). Unknown or missing = `ok`, logged as such."""
        try:
            line = (self.dir / "scenario").read_text().strip()
        except FileNotFoundError:
            line = "ok"
        word = line.split()[0] if line else "ok"
        return (word if word in SCENARIOS else "ok"), line

    def log(self, **event):
        event["t"] = round(time.time(), 3)
        with self.lock, open(self.dir / "events.jsonl", "a") as out:
            out.write(json.dumps(event, sort_keys=True) + "\n")

    def mock_address_role(self, qtype):
        """Which address role the mock name resolves to for this query, or
        None for an empty answer. Only A queries advance a rebinding
        sequence, so the resolver's parallel AAAA query cannot skew it."""
        word, line = self.scenario()
        if word in ADDRESS_SCENARIOS:
            return ADDRESS_SCENARIOS[word]
        if qtype != 1:
            return None
        if word == "rebind":
            # "rebind <generation> [public-answers]": the first N A answers
            # are public, every later one private. A new line restarts.
            parts = line.split()
            public_answers = int(parts[2]) if len(parts) > 2 and parts[2].isdigit() else 1
            with self.lock:
                if line != self.rebind_line:
                    self.rebind_line, self.rebind_answers = line, 0
                self.rebind_answers += 1
                count = self.rebind_answers
            return "public_mock" if count <= public_answers else "private_rfc1918"
        return "public_mock"


# --- DNS -----------------------------------------------------------------

def parse_question(packet):
    labels, offset = [], 12
    while True:
        length = packet[offset]
        if length == 0:
            offset += 1
            break
        if length & 0xC0:
            raise ValueError("compressed question")
        labels.append(packet[offset + 1:offset + 1 + length].decode("ascii").lower())
        offset += 1 + length
    qtype, qclass = struct.unpack("!HH", packet[offset:offset + 4])
    return ".".join(labels), qtype, qclass, offset + 4


def dns_answer(state, packet):
    (ident, _flags, qdcount) = struct.unpack("!HHH", packet[:6])
    if qdcount != 1:
        raise ValueError("one question only")
    name, qtype, qclass, end = parse_question(packet)
    names = state.network["names"]
    addresses = state.network["addresses"]
    role = None
    if name == names["mock"]:
        role = state.mock_address_role(qtype) if qtype in (1, 28) else None
    elif name == names["redirect_target"]:
        role = "redirect_target"
    elif name == names["wrong_name"]:
        role = "public_mock"
    known = name in names.values()
    rcode = 0 if known else 3
    answers = b""
    answered = None
    if role:
        address = addresses[role]
        family = socket.AF_INET6 if ":" in address else socket.AF_INET
        want = 28 if family == socket.AF_INET6 else 1
        if qtype == want and qclass == 1:
            rdata = socket.inet_pton(family, address)
            answers = struct.pack("!HHHIH", 0xC00C, qtype, 1, 0, len(rdata)) + rdata
            answered = role
    header = struct.pack("!HHHHHH", ident, 0x8180 | rcode, 1, 1 if answers else 0, 0, 0)
    state.log(kind="dns", name=name, qtype=qtype, answer_role=answered, rcode=rcode)
    return header + packet[12:end] + answers


def serve_dns(state, sock):
    while True:
        packet, peer = sock.recvfrom(512)
        try:
            sock.sendto(dns_answer(state, packet), peer)
        except (ValueError, IndexError, UnicodeDecodeError, struct.error):
            state.log(kind="dns", malformed=True)


# --- sinks ---------------------------------------------------------------

def serve_sink(state, role, sock):
    while True:
        conn, _peer = sock.accept()
        state.log(kind="connect", role=role)
        conn.close()


# --- HTTPS mock ----------------------------------------------------------

def tls_contexts(pki):
    def context(cert, key):
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.minimum_version = ssl.TLSVersion.TLSv1_2
        ctx.load_cert_chain(pki / cert, pki / key)
        return ctx
    return {
        "good": context("mock.crt", "mock.key"),
        "tls-untrusted": context("untrusted-mock.crt", "untrusted-mock.key"),
        "tls-wrong-name": context("wrong-name.crt", "wrong-name.key"),
    }


def read_request(conn):
    data = b""
    while b"\r\n\r\n" not in data:
        chunk = conn.recv(4096)
        if not chunk:
            return None
        data += chunk
        if len(data) > MAX_HEADER:
            return None
    head, _, body = data.partition(b"\r\n\r\n")
    lines = head.decode("latin-1").split("\r\n")
    method, path, _version = lines[0].split(" ", 2)
    headers = {}
    for line in lines[1:]:
        key, _, value = line.partition(":")
        headers[key.strip().lower()] = value.strip()
    length = int(headers.get("content-length", "0") or 0)
    if length > MAX_BODY:
        return None
    while len(body) < length:
        chunk = conn.recv(min(65536, length - len(body)))
        if not chunk:
            return None
        body += chunk
    return method, path, headers, body


def respond(conn, status, body=b"", headers=()):
    reasons = {200: "OK", 302: "Found", 401: "Unauthorized", 404: "Not Found",
               429: "Too Many Requests", 503: "Service Unavailable"}
    lines = [f"HTTP/1.1 {status} {reasons.get(status, 'Status')}", "Connection: close",
             f"Content-Length: {len(body)}", *headers]
    conn.sendall(("\r\n".join(lines) + "\r\n\r\n").encode() + body)


def json_body(value):
    return json.dumps(value).encode()


def chat_reply(scenario, model, prompt):
    text = SENTINEL_REPLY if SENTINEL_PROMPT in prompt else REPLY_TEXT
    message = {"role": "assistant", "content": text}
    usage = {"prompt_tokens": 9, "completion_tokens": 3, "total_tokens": 12}
    reported = model
    if scenario == "model-mismatch":
        reported = "mock-other-model"
    elif scenario == "tool-calls":
        message = {"role": "assistant", "content": None,
                   "tool_calls": [{"id": "call-1", "type": "function",
                                   "function": {"name": "mock_tool", "arguments": "{}"}}]}
    elif scenario == "empty-output":
        message["content"] = ""
    body = {"id": "mock-1", "object": "chat.completion", "model": reported,
            "choices": [{"index": 0, "message": message, "finish_reason": "stop"}]}
    if scenario != "missing-usage":
        body["usage"] = usage
    return body


def handle_http(state, args, conn):
    scenario, _ = state.scenario()
    request = read_request(conn)
    if request is None:
        state.log(kind="http", role="mock", scenario=scenario, malformed=True)
        return
    method, path, headers, body = request
    token = headers.get("authorization", "")
    if token.lower().startswith("bearer "):
        digest = hashlib.sha256(token[7:].strip().encode()).hexdigest()
        auth = "match" if digest == args.key_sha256 else "mismatch"
    else:
        auth = "absent"
    event = {"kind": "http", "role": "mock", "scenario": scenario, "method": method,
             "path": path.split("?")[0][:64], "auth": auth, "body_bytes": len(body)}
    chat = path in ("/v1/chat/completions", "/api/chat/completions") and method == "POST"
    models = path in ("/v1/models", "/api/models") and method == "GET"
    prompt = ""
    if chat:
        try:
            parsed = json.loads(body)
            prompt = "".join(m.get("content") or "" for m in parsed.get("messages", [])
                             if isinstance(m, dict) and isinstance(m.get("content"), str))
            event.update(model=str(parsed.get("model"))[:128], max_tokens=parsed.get("max_tokens"),
                         stream=parsed.get("stream"), tools_offered="tools" in parsed or "functions" in parsed)
        except (ValueError, AttributeError):
            event["unparsable_request"] = True
        # Lengths and markers only: the prompt itself is never logged.
        event.update(prompt_chars=len(prompt), sentinel_prompt_seen=SENTINEL_PROMPT in prompt)
    status, payload, extra = 404, b"", ()
    if auth != "match" or scenario == "auth-failure":
        status, payload = 401, json_body({"error": "unauthorized"})
    elif scenario == "rate-limited" and chat:
        # Discovery succeeds; the chat request is the one limited.
        status, payload, extra = 429, json_body({"error": "slow down"}), (f"Retry-After: {RETRY_AFTER_SECONDS}",)
    elif scenario == "server-error" and chat:
        status, payload = 503, json_body({"error": "unavailable"})
    elif scenario == "redirect":
        target = f"https://{args.network['names']['redirect_target']}:{args.network['mock_port']}{path}"
        status, extra = 302, (f"Location: {target}",)
    elif models:
        listed = [] if scenario == "missing-model" else [{"id": args.model, "object": "model"}]
        status, payload = 200, json_body({"object": "list", "data": listed + [{"id": "mock-other-model"}]})
    elif chat:
        if scenario == "delayed":
            time.sleep(DELAY_SECONDS)
        if scenario == "oversized":
            event["status"] = 200
            state.log(**event)
            conn.sendall(f"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\n"
                         f"Content-Length: {OVERSIZED}\r\n\r\n".encode() + b" " * OVERSIZED)
            return
        if scenario == "malformed-json":
            status, payload = 200, b'{"model": "truncated'
        elif scenario == "unknown-outcome":
            event.update(status=200, dropped_mid_body=True)
            state.log(**event)
            conn.sendall(b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 4096\r\n\r\n{\"model\":")
            return
        else:
            status, payload = 200, json_body(chat_reply(scenario, event.get("model", args.model), prompt))
    event["status"] = status
    state.log(**event)
    respond(conn, status, payload, ("Content-Type: application/json", *extra))


def serve_mock(state, args, sock, contexts):
    def worker(raw):
        scenario, _ = state.scenario()
        context = contexts.get(scenario, contexts["good"])
        state.log(kind="connect", role="mock", scenario=scenario)
        try:
            with context.wrap_socket(raw, server_side=True) as conn:
                conn.settimeout(30)
                handle_http(state, args, conn)
        except (ssl.SSLError, OSError) as err:
            state.log(kind="tls", role="mock", scenario=scenario, error=type(err).__name__)
        finally:
            raw.close()
    while True:
        raw, _peer = sock.accept()
        threading.Thread(target=worker, args=(raw,), daemon=True).start()


# --- startup -------------------------------------------------------------

def bind(kind, spec):
    host, _, port = spec.rpartition(":")
    host = host.strip("[]")
    family = socket.AF_INET6 if ":" in host else socket.AF_INET
    sock = socket.socket(family, kind)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    if "%" in host:
        address, scope = host.split("%")
        sock.bind((address, int(port), 0, socket.if_nametoindex(scope)))
    else:
        sock.bind((host, int(port)))
    if kind == socket.SOCK_STREAM:
        sock.listen(64)
    return sock


def guest_listeners(network):
    """Every role on its plan address: the mock on the public address, a
    sink on every other one (the metadata address also on port 80)."""
    port = network["mock_port"]
    listen = {}
    for role, address in network["addresses"].items():
        if role in ("loopback_v6",):
            continue  # [::1]:22 is sshd's; providerd's sink is on v4 loopback.
        if role == "link_local_v6":
            address = f"{address}%{network['interface']}"
        listen[role] = f"[{address}]:{port}" if ":" in address else f"{address}:{port}"
    listen["metadata_v4_http"] = f"{network['addresses']['metadata_v4']}:80"
    listen["mock"] = listen.pop("public_mock")
    return listen


def main(argv):
    parser = argparse.ArgumentParser(description="Flowerly KVM acceptance mock (TEST ONLY)")
    parser.add_argument("--network", required=True, type=pathlib.Path)
    parser.add_argument("--pki", required=True, type=pathlib.Path)
    parser.add_argument("--state", required=True, type=pathlib.Path)
    parser.add_argument("--model", required=True)
    parser.add_argument("--key-sha256", required=True)
    parser.add_argument("--listen", action="append", default=[],
                        help="role=address:port; replaces the plan's listeners (offline tests)")
    parser.add_argument("--dns", help="address:port for the DNS server (default: the plan's stub, port 53)")
    args = parser.parse_args(argv[1:])
    args.network = json.loads(args.network.read_text())
    if len(args.key_sha256) != 64:
        sys.exit("--key-sha256 must be a SHA-256 hex digest")
    state = State(args.state, args.network)
    listen = dict(spec.split("=", 1) for spec in args.listen) if args.listen else guest_listeners(args.network)
    contexts = tls_contexts(args.pki)
    bound = {}
    threads = []
    for role, spec in listen.items():
        sock = bind(socket.SOCK_STREAM, spec)
        bound[role] = list(sock.getsockname()[:2])
        target = serve_mock if role == "mock" else serve_sink
        extra = (args, sock, contexts) if role == "mock" else (role, sock)
        threads.append(threading.Thread(target=target, args=(state, *extra), daemon=True))
    dns_sock = bind(socket.SOCK_DGRAM, args.dns or f"{args.network['dns_stub']}:53")
    bound["dns"] = list(dns_sock.getsockname()[:2])
    threads.append(threading.Thread(target=serve_dns, args=(state, dns_sock), daemon=True))
    for thread in threads:
        thread.start()
    ports = args.state / "ports.json"
    ports.write_text(json.dumps(bound, sort_keys=True) + "\n")
    os.chmod(ports, 0o644)
    while all(thread.is_alive() for thread in threads):
        time.sleep(0.5)
    sys.exit("a mock listener stopped")


if __name__ == "__main__":
    sys.exit(main(sys.argv))
