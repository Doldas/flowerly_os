# Reference inference provider (RFC 0004 tranche 4)

An [opt-in OpenAI-compatible external API probe](openai-compatible-provider-probe.md)
is available for offline/mock and operator-authorized manual testing. It is **not**
connected to providerd and does not relax ADR-0020's remote-egress prohibition.

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
4. Calls `provider.generate` over the authenticated local IPC.
5. On success writes **one bounded ledger row** (the only durable artifact)
   with the client `operation_id` — retries replay the identical row (#63
   semantics) and conflicts return typed `Conflict`.
6. On provider outage, honors the profile's explicit fallback policy:
   - `ask` (default) → `{ executed: false, requires_consent: true, policy: "ask" }` — no retry, no state change.
   - `queue` → same shape with `policy: "queue"`; the workload stays ADMITTED untouched.
   - `fail` → typed `Unavailable`.
   - `downgrade` → typed `PermissionDenied`: silently substituting a different
     model would be exactly what RFC 0004 forbids.

The provider response **cannot grant the workload any OS authority**; it is
data. Flow/Session state is never modified on this path. Cost is recorded at
zero for the reference provider; real providers supply accounting later.

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
- `providerd` is dev/CI infrastructure and not part of the distribution
  image manifest.
- Rate limiting is per actor in-flight concurrency (4) only; no scheduling,
  priority eviction or multi-tenant accounting yet (see
  [compute-scheduling](compute-scheduling.md) honest limitations).
