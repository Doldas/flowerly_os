#!/usr/bin/env python3
"""Opt-in OpenAI-compatible API probe; not wired into Flowerly's providerd."""
import argparse
import json
import os
import ssl
import sys
import urllib.error
import urllib.parse
import urllib.request

MAX_BODY = 1024 * 1024


class ProbeError(Exception):
    pass


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ProbeError("HTTP redirect refused (credential forwarding prohibited)")


def endpoint(base, path, allow_http=False):
    parsed = urllib.parse.urlsplit(base)
    if parsed.scheme not in ({"https", "http"} if allow_http else {"https"}):
        raise ProbeError("HTTPS required (HTTP allowed only for explicit local mock tests)")
    if not parsed.hostname or parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise ProbeError("base URL must be an origin/path without credentials, query or fragment")
    if parsed.scheme == "http" and parsed.hostname not in {"127.0.0.1", "localhost", "::1"}:
        raise ProbeError("insecure HTTP is only permitted on loopback")
    prefix = parsed.path.rstrip("/")
    if prefix.endswith("/v1"):
        prefix = prefix[:-3]
    if prefix.endswith("/api"):
        prefix = prefix[:-4]
    if prefix not in {"", "/"}:
        raise ProbeError("base URL path must be /, /v1 or /api")
    return urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, path, "", ""))


def call(url, key, payload=None, timeout=20):
    headers = {"Authorization": "Bearer " + key, "Accept": "application/json"}
    if payload is not None:
        headers["Content-Type"] = "application/json"
    data = None if payload is None else json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(url, data=data, headers=headers, method="POST" if data else "GET")
    opener = urllib.request.build_opener(NoRedirect, urllib.request.HTTPSHandler(context=ssl.create_default_context()))
    try:
        with opener.open(req, timeout=timeout) as response:
            raw = response.read(MAX_BODY + 1)
            if len(raw) > MAX_BODY:
                raise ProbeError("response exceeds 1 MiB limit")
            return json.loads(raw)
    except urllib.error.HTTPError as exc:
        raise ProbeError(f"server returned HTTP {exc.code}") from None
    except (urllib.error.URLError, TimeoutError, OSError):
        raise ProbeError("connection or TLS failure (details suppressed to protect credentials)") from None
    except (UnicodeError, ValueError):
        raise ProbeError("invalid JSON response") from None


def probe(base, key, model, api_style="openai", timeout=20, chat=True):
    if api_style == "openai":
        models_path, chat_path = "/v1/models", "/v1/chat/completions"
    else:
        models_path, chat_path = "/api/models", "/api/chat/completions"
    models = call(endpoint(base, models_path), key, timeout=timeout)
    ids = [x.get("id") for x in models.get("data", []) if isinstance(x, dict)] if isinstance(models, dict) else []
    if model not in ids:
        raise ProbeError("requested model not present in discovered model IDs")
    if not chat:
        return {"discovery": "ok", "model_present": True, "chat": "not_requested"}
    response = call(endpoint(base, chat_path), key, {"model": model, "messages": [{"role": "user", "content": "Reply with exactly: flowerly-ready"}], "max_tokens": 64, "stream": False}, timeout)
    choices = response.get("choices", []) if isinstance(response, dict) else []
    if not choices or not isinstance(choices[0], dict) or not isinstance(choices[0].get("message"), dict):
        raise ProbeError("chat response missing expected choices[0].message")
    content = choices[0]["message"].get("content")
    if not isinstance(content, str) or not content:
        raise ProbeError("chat response has no nonempty text (model may use reasoning-only tokens)")
    return {"discovery": "ok", "model_present": True, "chat": "ok", "text_present": True}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", required=True)
    parser.add_argument("--model", default="gpt-oss:20b")
    parser.add_argument("--api-style", choices=("openai", "open-webui"), default="openai")
    parser.add_argument("--timeout", type=int, default=30)
    parser.add_argument("--discover-only", action="store_true")
    args = parser.parse_args()
    if args.timeout < 1 or args.timeout > 120:
        parser.error("timeout must be 1..120 seconds")
    key = os.environ.get("FLOWERLY_PROBE_API_KEY", "")
    if not key or "\r" in key or "\n" in key:
        parser.error("set FLOWERLY_PROBE_API_KEY to a valid secret (never pass on command line)")
    try:
        print(json.dumps(probe(args.base_url, key, args.model, args.api_style, args.timeout, not args.discover_only)))
    except ProbeError as exc:
        print(f"probe failed: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
