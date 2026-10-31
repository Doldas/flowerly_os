#!/usr/bin/env python3
"""Offline loopback mock acceptance for the opt-in API probe."""
import importlib.util
import json
import pathlib
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

path = pathlib.Path(__file__).with_name("probe-openai-compatible.py")
spec = importlib.util.spec_from_file_location("flowerly_probe", path)
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        self.respond({"data": [{"id": "gpt-oss:20b"}]})

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if body["model"] != "gpt-oss:20b" or body["stream"] is not False:
            self.send_error(400)
            return
        self.respond({"choices": [{"message": {"content": "flowerly-ready"}}]})

    def respond(self, value):
        data = json.dumps(value).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *_):
        pass


class ProbeTests(unittest.TestCase):
    def test_secure_url_validation(self):
        self.assertEqual(probe.endpoint("https://example.org/v1", "/v1/models"), "https://example.org/v1/models")
        self.assertEqual(probe.endpoint("https://example.org/api", "/api/models"), "https://example.org/api/models")
        for url in ("http://example.org", "https://user:pass@example.org", "https://example.org/?key=secret", "file:///tmp/a", "https://example.org/evil"):
            with self.assertRaises(probe.ProbeError):
                probe.endpoint(url, "/v1/models")

    def test_offline_mock(self):
        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            base = f"http://127.0.0.1:{server.server_port}"
            original = probe.endpoint
            probe.endpoint = lambda b, p: original(b, p, allow_http=True)
            self.assertEqual(probe.probe(base, "test-key", "gpt-oss:20b")["chat"], "ok")
            with self.assertRaises(probe.ProbeError):
                probe.probe(base, "test-key", "wrong-model")
        finally:
            probe.endpoint = original
            server.shutdown()
            server.server_close()


if __name__ == "__main__":
    unittest.main()
