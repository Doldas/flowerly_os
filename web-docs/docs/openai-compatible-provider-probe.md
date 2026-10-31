# OpenAI-compatible inference: opt-in external probe

Status: **offline-tested exploratory tool, not a Flowerly provider** (2026-10-29).

Flowerly currently has a synthetic, local Unix-socket `providerd`. ADR-0020
**forbids remote provider egress** until an authorized egress policy gate has
been designed, reviewed and tested. ADR-0021 documents `providerd` as a
no-network reference. This tool deliberately does **not** change those
contracts or connect remote inference to `flowerlyd`.

## Test the user's server without sharing a key

The user supplied `https://openui.gamitronservers.com/` as an endpoint; the
actual API path and exact model ID must be verified, not assumed. The probe
supports both OpenAI `/v1/*` and Open WebUI `/api/*` routes. It uses a
non-admin, least-privilege key loaded from an environment variable, refuses
HTTP except loopback in the offline tests, rejects redirects (no credential
forwarding), validates TLS, caps responses to 1 MiB and never prints model
outputs or the key. Its live request sends a fixed non-sensitive test prompt.

```sh
# Do this locally, not in GitHub Actions or in a chat message.
read -rs FLOWERLY_PROBE_API_KEY
export FLOWERLY_PROBE_API_KEY
python3 scripts/probe-openai-compatible.py \
  --base-url https://openui.gamitronservers.com/ \
  --api-style openai --model 'gpt-oss:20b'
unset FLOWERLY_PROBE_API_KEY
```

If the server exposes Open WebUI's native API instead, retry with
`--api-style open-webui`. Use `--discover-only` to avoid sending a chat
request. A nonzero exit status means the probe did not verify the contract.

The command only reports whether model discovery and a chat completion worked;
no raw output is logged. It is an **opt-in egress probe**, run only on a
trusted machine where the operator authorizes sending the fixed prompt and
credential to this server. Do not place the key in a shell command, repository,
CI secret or Generation manifest. The `read` example leaves the key in the
shell environment until `unset`; prefer a dedicated, short-lived shell and
revoke the test key after use. For production, prefer systemd credentials.

## Offline acceptance

```sh
python3 scripts/test-openai-compatible-probe.py
```

The test uses a loopback mock, with no remote network or credentials. It
covers model discovery, nonstreaming chat shape, missing model, and URL
validation. This is **not** evidence that the remote server is reachable or
that `gpt-oss:20b` is installed.

## Required before integration with Flowerly

1. New ADR and threat-model review for a remote inference egress boundary,
   DNS rebinding/SSRF, TLS verification, explicit host allowlist, credential
   scoping, redirects, log redaction and network isolation. No arbitrary URL
   accepted from untrusted model content or user workloads.
2. Choose a fault-isolated, bounded worker interface without blocking the
   supervisor's synchronous IPC or database lock for an HTTP timeout.
3. Enforce per-user permission, per-profile consent for off-machine prompt
   transfer, quotas, cancellation, request identity and bounded response
   parsing. Remote inference has unknown-outcome retry semantics; never
   silently duplicate billable calls.
4. Map actual server-reported usage and measured latency honestly; missing
   usage must be `unknown`, not fabricated. Reject mismatched model IDs.
5. Add adversarial mock tests for redirects, oversize JSON, timeouts,
   401/429/5xx, malformed responses, secret redaction, consent and outage.
6. Keep system action authority behind Flowerly's OS-enforced checks; model
   output is untrusted data and cannot promote or roll back generations.

No live call has been performed by this PR, and no real model is claimed to
run inside Flowerly yet.
