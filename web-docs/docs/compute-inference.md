# Reference inference provider (RFC 0004 tranche 4)

Status: **prototype surface, fourth tranche of RFC 0004**; completes the local
provider execution contract under [ADR-0020](adr/0020-ai-compute-layer-is-a-module.md)
and [ADR-0021](adr/0021-provider-fault-isolation.md). No real models are
executed anywhere in the system; this reference is synthetic, honest and
fail-closed.

## Process boundary

`providerd` is a **separate process** next to `flowerlyd`, speaking the same
authenticated local Unix-socket IPC (ADR-0010). The separation is deliberate
fault isolation (an ADR-0013 criterion): a provider outage must never corrupt
or block the Flowerly runtime — only the `Unavailable` fallback path is
exercised. It is a reference, not provisioned into the distro image manifest
(kept out of `distro-builder` validation until real provider deployments
are authorized).

## Reference provider contract

| Command | Behavior |
|---|---|
| `provider.discover` | Honest capabilities: `synthetic: true`, `streaming: false`, bounded token/prompt caps, per-actor in-flight cap, `promise: reference-contract-only`. |
| `provider.generate` | Deterministic bounded echo; request_id echoed verbatim; latency measured; `FLOWERLY_PROVIDER_OUTAGE=1` fails every request (outage seam). |

## Execution flow through flowerlyd

`inference.request` in the compute module:

1. Loads the workload; **ADMITTED only** — other states are typed `Conflict`
   (never silent execution).
2. Bounds prompt (16 KiB) and max_tokens (1..4096) at the runtime too.
3. Resolves the provider row and its **runtime-dir-local** socket endpoint
   (endpoints outside the runtime directory are `PermissionDenied`).
4. With an `operation_id`, looks up a recorded response **before** calling
   the provider: a retry replays it without a second execution (a repeated
   provider call would repeat its external effect and any cost), and a reused
   identity with different arguments is a typed `Conflict` (#263).
5. Calls `provider.generate` over the authenticated local IPC **without the
   shared store lock held**: steps 1–4 and 6 each take the lock briefly, so a
   slow provider cannot stall other modules' requests. (The compute module's
   own queue is still serial and bounded by the 7 s module deadline; real
   remote inference needs the asynchronous design tracked in #263.)
6. On success writes **one bounded ledger row** (the only durable artifact)
   with the client `operation_id` — the write re-checks the identity
   atomically, so a response recorded meanwhile wins (#63 semantics).
7. On provider outage, honors the profile's explicit fallback policy:
   - `ask` (default) → `{ executed: false, requires_consent: true, policy: "ask" }` — no retry, no state change.
   - `queue` → same shape with `policy: "queue"`; the workload stays ADMITTED untouched.
   - `fail` → typed `Unavailable`.
   - `downgrade` → typed `PermissionDenied`: silently substituting a different
     model would be exactly what RFC 0004 forbids.

The provider response **cannot grant the workload any OS authority**; it is
data. Flow/Session state is never modified on this path. Cost is recorded at
zero for the reference provider; real providers supply accounting later.

## Opt-in OpenAI-compatible backend (not reachable from flowerlyd)

`providerd --remote-config <file>` serves an OpenAI-compatible HTTPS backend
instead of the echo backend. It implements the provider half of the
**proposed** ADR-0022 (#265, tracked in #263). flowerlyd still refuses to
register any remote provider kind, so no Flowerly workload can reach it
until that ADR is accepted and its administrator, profile and consent gates
are wired.

The administrator-owned configuration (unknown fields are refused, and
limits can only be lowered):

```toml
[remote]
api_base = "https://inference.example.org"   # https origin only
api_style = "openai"            # /v1/*; or "open-webui" for /api/*
models = ["gpt-oss:20b"]        # allowlist; checked against discovery
credential = "inference.key"    # name inside $CREDENTIALS_DIRECTORY
max_tokens_cap = 1024           # <= 4096
connect_timeout_ms = 5000       # <= 5000
request_timeout_ms = 60000      # <= 120000
max_concurrency = 2             # <= 16
allow_private_addresses = false # true only for a self-hosted LAN server
```

The API key is read only from systemd credentials
(`LoadCredentialEncrypted=` or `LoadCredential=`); there is no
environment, argument or config-file path for it. `providerd
--remote-config <file> --self-test` validates the configuration, the
credential and the system trust store without making any request.

| Command | Behaviour |
| --- | --- |
| `provider.discover` | live `/models` call (≤ 8 s); returns the allowlisted models the server lists |
| `provider.submit` | validates, then returns `pending` at once; idempotent per `(created_by, request_id)` |
| `provider.poll` | `pending`, `succeeded`, `failed` or `unknown`, with error `class` and `outcome` |
| `provider.cancel` | best-effort; after the request was sent the outcome is `unknown` |
| `provider.generate` | refused: the remote backend is asynchronous only |

Each failure carries `outcome`. `not_sent` (DNS, connect, TLS,
address policy, discovery, local limits) is safe to retry. `sent` means
the server answered (401/403, 404, 429, redirect, invalid or tool-call
responses, model mismatch). `unknown` (5xx, timeout after send, transport
loss, cancellation after send, lost in a restart) may have executed and
been billed, and is never retried by the provider.

Fail-closed properties covered by tests:

- TLS against the system trust store only. `SSL_CERT_FILE`/`SSL_CERT_DIR`
  refuse startup, and no flag disables verification.
- Redirects are never followed, and the proxy environment is ignored.
- Only the configured host is resolved. Loopback, link-local, metadata,
  documentation and (without opt-in) private addresses are refused inside
  the resolver that the connection uses.
- A 401/403 opens a circuit until restart.
- Response body ≤ 1 MiB; returned text ≤ 32 KiB (`truncated` flagged).
- Usage is `provider_reported` or `unknown`, never estimated.
- A response naming a different model fails as `model_mismatch` with the
  text withheld.
- Tool or function calls are refused.
- The key never appears in replies, errors or logs.

Model output is returned as an inert string and grants nothing.

## Security boundaries

- The provider is remote-hostile by construction: local socket only, single
  user enforced by the core runner (peer UID must own the socket), no network
  transport, no credentials, no egress, nothing on disk.
- A poisoned runtime lock or IPC failure is an internal outage path, never a
  silent success; ledger rows only land after an OK provider result.
- Restarting either process is durable: ledger rows and usage totals
  survive; workloads and admissions persist through supervisor restarts.

## Honest limitations

- No real model execution, streaming, or GPU inference exists; the reference
  output is bounded synthetic echo.
- Remote providers are refused unless an administrator enables them under
  [ADR-0022](adr/0022-remote-inference-egress-policy.md) (#263):
  administrator, profile and per-user consent gates, with egress confined
  to one provider process.
- `providerd` is dev/CI infrastructure and not part of the distribution
  image manifest.
- Rate limiting is per actor in-flight concurrency (4) only; no scheduling,
  priority eviction or multi-tenant accounting yet (see
  [compute-scheduling](compute-scheduling.md) honest limitations).
