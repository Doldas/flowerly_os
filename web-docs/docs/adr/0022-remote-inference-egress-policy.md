# 0022: Opt-in Remote Inference Through an Egress-Confined Provider Process

Date: 2026-10-29 (revised 2026-10-30 with the owner's direction on Q1–Q4)
Status: **Accepted** (2026-10-30, by the owner, under the conditions in
[Acceptance](#acceptance-2026-10-30)). Acceptance permits the gated code
path; it does not enable egress. Remote inference stays disabled until an
administrator installs the provider unit and gate policy, and the first
live request needs its own explicit owner approval.
Issue: [#263](https://github.com/Doldas/flowerly_os/issues/263).
Planning context: [#64](https://github.com/Doldas/flowerly_os/issues/64),
[RFC 0004](../rfcs/0004-location-independent-flowerly-ai-compute.md).
Not 0.0.4 scope.

## Context

ADR-0020 keeps remote provider kinds and all egress structurally refused
"until an authorized egress policy gate with explicit review exists".
ADR-0021 requires every provider to be bounded, local-socket only, without
egress or credentials, "or earn a new ADR with evidence". This is that ADR
for one provider kind: an OpenAI-compatible HTTPS API (initial model
`gpt-oss:20b`, operator-hosted). The exploratory operator probe (#262)
proved nothing about the runtime and never ran against a live server.

### Gap analysis (release/2 @ `0ac112b`)

| # | Observed | Consequence for remote inference |
| --- | --- | --- |
| G1 | `provider.register` refuses every kind except `local-unix` (`PermissionDenied`); endpoints must be bare filenames in flowerlyd's runtime directory | No remote provider can be registered (correct until this ADR is accepted) |
| G2 | `compute::handle_command` held the shared `Mutex<Store>` across the blocking provider IPC | A slow remote call stalls every module. Fixed by #264 |
| G3 | flowerlyd module deadline 7 s; IPC request deadline 10 s, also inside providerd's own `run_service` | A synchronous request cannot carry real inference; a cold 20B model exceeds both |
| G4 | `operation_id` replay was checked after provider execution | A retry was re-executed and billed twice. Fixed by #264 |
| G5 | `inference_usage.tokens_in/out` are non-null; `cost_micros` is always 0 | Missing server usage cannot be recorded as *unknown* without a migration |
| G6 | `ipc_operations` stores the full request (prompt) and response for 30 days | A prompt sent off-machine is also retained locally |
| G7 | `Locality::ApprovedRemoteOptIn` exists but is recorded only; nothing checks it | No consent enforcement exists |
| G8 | No credential path, no providerd unit, no network restriction on `flowerlyd.service` | Nowhere safe to put a key; nothing structural prevents egress from the supervisor |
| G9 | No HTTP/TLS stack in the workspace | A supply-chain decision (licenses, advisories) is required |

## Decision

### 1. Egress lives in exactly one process kind

Only a `providerd` instance started with the `openai-compatible` backend may
open network connections. flowerlyd, evolutiond and the synthetic echo
provider never do. Each remote provider runs as its own systemd service with
its own network allowance. flowerlyd reaches it only over the authenticated
local IPC, as today. A compromise of HTTP/JSON parsing is contained to that
process: it holds no database handle and no OS authority.

The remote provider runs under a dedicated account (`flowerly-provider`),
not `flowerly`, so it cannot read `/var/lib/flowerly/state.db`. Its socket
lives in its own `RuntimeDirectory` with group access for `flowerly`.
flowerlyd accepts that endpoint only from its root-owned configuration,
never from IPC arguments (see §2). The provider admits only the service
account and the `flowerly-provider-clients` group, whose members are
`flowerly` and the provider itself. **Owner Q1: approved.** The account is
dedicated and least-privilege, with no access to `state.db` or to
unrelated credentials.

### 2. Every gate is explicit and fails closed

A prompt leaves the machine only when **all** of the following hold. Each
refusal is a typed error and is tested.

1. **Administrator, at the host.** A root-owned provider configuration file
   (not IPC, not environment) names the provider and sets:
   - `api_base` (an `https://` origin, no userinfo, query or fragment);
   - `api_style` (`openai` → `/v1/models` and `/v1/chat/completions`; or
     `open-webui` → `/api/models` and `/api/chat/completions`);
   - the exact model allowlist;
   - limits and timeouts;
   - the socket path.

   flowerlyd reads its own gate policy only from a path given on its
   command line (`--remote-providers`). The file must not be a symlink,
   must be owned by root or the service account, and must not be group-
   or world-writable. It must say `enabled = true` and list each provider's
   socket, destination host and model allowlist. Unknown fields are
   refused, so it cannot carry a credential. If the file is absent or
   fails any check, remote inference is disabled with a stated reason, and
   local administration is unaffected.
2. **Administrator, registration.** `provider.register` (Admin class) for
   kind `remote-openai-compatible` succeeds only when step 1 holds. The
   registered destination host is recorded and shown by `provider.show`.
3. **Profile.** The workload's resolved profile has
   `locality = "approved-remote-opt-in"`. `local-only` (the default, and
   what inheritance tightens toward) refuses with `PermissionDenied`.
4. **User consent (owner Q3: approved with conditions).** The
   authenticated user (kernel-attested uid) holds an unrevoked grant for
   that provider id *and* destination host, lasting at most 30 days and
   revocable immediately. The user must name the destination to grant it.
   The grant's scope is immutable (database trigger). Changing the
   provider or the configured destination leaves no matching grant.
   Consent is checked under the store lock before the attempt is recorded
   or anything is sent. Each request also carries `remote: true`. Refusing
   or revoking consent leaves local administration fully usable.
   Revocation stops new prompts at once; it cannot recall an attempt that
   was already submitted.
5. **Provider identity.** Before the prompt is submitted, the provider
   process must report over IPC the same destination the policy and the
   consent name. Otherwise the attempt fails as `not_sent` /
   `destination_mismatch`.
6. **No silent placement change.** A local-to-remote or remote-to-local
   substitution never happens as a fallback. `downgrade` stays refused, as
   in ADR-0021.

### 3. Network policy (providerd remote backend)

- **TLS:** HTTPS only. Certificates are verified against the system trust
  store (Arch `ca-certificates`) with TLS ≥ 1.2. No code path, flag or
  configuration disables verification. A TLS failure is a typed failure and
  never a downgrade to HTTP. Plain HTTP exists only in test builds bound to
  loopback mock servers.
- **Fixed destinations:** the only reachable URLs are `api_base` joined with
  the two fixed paths of the configured `api_style`. Model output, user
  input and server responses can never supply a URL.
- **Redirects:** refused. Any 3xx is a typed `redirect_refused` failure, and
  the `Authorization` header is never forwarded.
- **Proxies:** proxy environment variables are ignored by construction and
  unset in the unit. An explicit administrator proxy is out of scope here.
- **SSRF and DNS rebinding:** the resolver used for the request is the one
  that filters addresses, so the addresses checked are the addresses
  connected to. It answers only for the configured host, and it runs for
  every new connection, so a later answer that rebinds the name is
  filtered like the first. It always refuses loopback, link-local
  (including 169.254.169.254), unspecified, multicast, broadcast,
  documentation, NAT64 and IPv4-compatible ranges.
- **Private destinations (owner Q2: conditional).** RFC 1918, CGNAT and
  IPv6 ULA addresses are denied by default. The only exception is an
  explicit, owner-approved `private_destination_cidrs` entry on that one
  provider. Entries are restricted to private blocks no wider than /16
  (IPv4) or /48 (IPv6), so an exception cannot open loopback, metadata or
  public space, and a rebinding answer outside the pinned block is
  refused. Verification on 2026-10-30: `openui.gamitronservers.com`
  resolves only to public Cloudflare anycast addresses (188.114.96.1,
  188.114.97.1, 2a06:98c1:3120::1, 2a06:98c1:3121::1). The system
  resolver and two public resolvers (1.1.1.1, 8.8.8.8) agree, so no
  private exception is needed.
- **systemd confinement** (defence in depth; the application checks above
  are authoritative):
  - remote provider unit: `RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6`
    and `IPAddressDeny=` for link-local and multicast, plus localhost unless
    the host's DNS resolver listens there (a local stub such as
    systemd-resolved at 127.0.0.53 must stay reachable, or every lookup
    fails closed). `IPAddressDeny=any` with `IPAddressAllow=` the
    destination's CIDRs is preferred when its addresses are stable;
  - `flowerlyd.service`: gains `IPAddressDeny=any` only after Arch VM
    acceptance proves nothing in it needs IP (tracked separately).

### 4. Credentials

- The API key comes only from systemd credentials
  (`LoadCredentialEncrypted=`, or `LoadCredential=` from a root-only file),
  read once at startup from `$CREDENTIALS_DIRECTORY/<provider>.key`.
  Rotation means updating the credential and restarting the unit.
- The key is never accepted from argv, the environment, a configuration
  file, IPC or the database.
- The key is validated (bounded length, visible ASCII only) and kept in a
  type whose `Debug` and `Display` print `[redacted]`. It is sent only as
  `Authorization: Bearer` to the configured origin.
- Error and log text never includes request headers or raw response
  bodies. Server error details are reduced to the HTTP status and an
  allowlisted error class.
- Tests assert that a sentinel key never appears in any error, log line,
  IPC response or database row.

### 5. Bounds and budgets

| Bound | Value (configurable downward only) |
| --- | --- |
| Prompt | 16 KiB (both processes, as today) |
| `max_tokens` | ≤ provider config cap ≤ 4096 |
| Response body | 1 MiB, enforced while streaming (`Content-Length` checked first) |
| Returned text | 32 KiB, held only in the provider's memory (§10) |
| Discovery list | ≤ 1024 models |
| Connect timeout | 5 s |
| Total timeout | ≤ 120 s per request |
| Concurrency | per provider (default 2) and per actor (existing 4); excess is `Unavailable`, never queued without bound |

`max_tokens` is sent as a request and not enforced by Flowerly beyond the
response bounds above. *Observation after acceptance (2026-10-30, #268):*
the operator's server reported 44 completion tokens for `max_tokens: 16`.
No remote token or spending cap is claimed. Results report
`max_tokens_requested` and `max_tokens_exceeded`.

Requests are non-streaming. Responses carrying `tool_calls`, function calls
or any non-text content are refused as `unsupported_response`: no tools are
offered, so none may come back.

### 6. Asynchronous execution and unknown outcomes

*As implemented (#267):* `inference.request` returns the durable attempt,
and `inference.result {attempt_id}` polls it. Attempts are owner-scoped,
and a database trigger makes a final attempt (`succeeded`, `failed` or
`unknown`) immutable.

Remote inference cannot fit the 7 s module deadline or the 10 s IPC
deadline (G3), so it uses submit and poll:

- **providerd:** `provider.submit` returns at once. It is idempotent per
  `(actor, request_id)` while the job is retained.
  `provider.poll` reports `pending`, `succeeded`, `failed` (with the
  `sent` / `not_sent` distinction) or `unknown`. `provider.cancel` cancels
  best-effort.
- **flowerlyd:** for a remote provider, `inference.request` durably records
  an *attempt* (owner, workload, provider, destination, consent, operation
  id, request digest, state `submitting`) **before** submitting. It then
  returns `{executed: false, attempt}` with the attempt `pending`.
  `inference.result` polls the attempt, and its final state is recorded
  exactly once.
- **Outcome classes.** A failure *before the request was sent* (DNS,
  connect refused, TLS handshake, local limits, open circuit) is
  `not_sent`, and the client may safely retry. A failure *after* sending
  (timeout, connection reset, providerd restart, cancellation after send)
  is **`unknown`**: the server may have executed and billed it. flowerlyd
  never resubmits an `unknown` attempt. A retry with the same `operation_id`
  returns the same `unknown` state. Running it again needs a new operation
  identity, which the response labels as a possible duplicate charge.
- **Restart:** if providerd restarts with jobs in flight, those attempts
  become `unknown`. If flowerlyd restarts, an open attempt is reconciled by
  the next `inference.result`: anything providerd no longer knows becomes
  `unknown`.

### 7. Accounting and provenance

- Remote usage is recorded on the attempt row (migration 0014, with the
  downgrade guard of #247): `usage_source ∈ {provider_reported, unknown}`
  and nullable token counts. Missing or malformed `usage` is recorded as
  `unknown`, never estimated. Local synthetic usage is unchanged.
- Latency is measured with a monotonic clock around the HTTP exchange in
  providerd, and separately end-to-end in flowerlyd.
- `cost_micros` is `NULL` (unknown) unless the administrator configures
  pricing. Nothing is derived from guesses.
- **Model identity:** the response's `model` must equal the requested
  allowlisted id. A mismatch fails closed as `model_mismatch`: the text is
  withheld, and the attempt is recorded as `failed` with outcome `sent`
  (cost may have been incurred).
- Provenance links the provider, workload, attempt and usage row. The
  registered destination host is part of the attempt record.

### 8. Failure mapping (fail closed)

| Condition | Code | Outcome class | Provider state |
| --- | --- | --- | --- |
| 401 / 403 | `PermissionDenied` (`auth_failed`) | sent | circuit **open** until restart; no retry |
| Model absent from discovery / 404 | `NotFound` (`model_unavailable`) | not_sent / sent | — |
| Model mismatch in response | `Conflict` (`model_mismatch`) | sent, text withheld | — |
| 429 | `Unavailable` (`rate_limited`, bounded `retry_after_ms`) | sent, not executed | no automatic retry |
| 5xx | `Unavailable` (`server_error`) | **unknown** | — |
| DNS / connect refused / address policy | `Unavailable` / `PermissionDenied` | not_sent | — |
| TLS failure | `Unavailable` (`tls`) | not_sent | never downgraded |
| Timeout after send / reset | `Unavailable` (`timeout`) | **unknown** | — |
| 3xx | `PermissionDenied` (`redirect_refused`) | sent | — |
| Oversized / malformed / tool calls | `Internal` (`invalid_response` / `unsupported_response`) | sent, text withheld | — |

The profile fallback policy (`ask` / `queue` / `fail`) applies only to
`not_sent` and rate-limited outcomes. An `unknown` outcome is reported as
unknown and never converted into a fallback execution.

### 9. Model output is untrusted data

Generated text is returned as a JSON string and held only in the provider
process's memory (§10). It is never parsed into commands, paths, URLs,
capabilities, profile changes or provider configuration, and it grants no OS
authority (ADR-0014, ADR-0020). Clients that print it to a terminal must
neutralise control sequences.

### 10. Privacy and retention (owner Q4: no default raw retention)

These rules are binding (owner Q4) and implemented in #267:

- **No raw retention.** Remote prompts and generated text are never
  written to the database, a log or any other persistent store, by
  default or otherwise. The attempt schema has no column that can hold
  either, and the idempotent-operation table is not used for remote
  requests.
- **Metadata and digests only.** An attempt keeps what idempotency,
  outcome classification and accounting need: a SHA-256 digest of the
  request, owner, workload, provider, destination, consent, model,
  state, outcome, error class, usage (reported or unknown) and latency.
- **Bounded, memory-only replay.** The generated text exists only in the
  provider process's memory and is delivered once by default. An
  administrator may set `response_replay_seconds`, at most **900**, to
  re-deliver it for that long after the first delivery. A persistent
  replay would need its own decision, with encryption under a key not
  stored beside it.
- **Explicit unavailability.** When the text can no longer be delivered,
  `inference.result` (and a retry with the same `operation_id`) returns
  `response_available: false` and `resubmitted: false` with the metadata.
  A digest cannot reproduce a response.
- **No automatic retry of an unknown outcome.** An `unknown` attempt is
  final. Running the request again needs a new `operation_id`, and the
  reply warns of a possible duplicate charge.
- **Logs** carry the provider name, destination host, request id, status
  class and latency: never prompt text, output text, headers, keys or
  the owner's uid.

### 11. Relationship to ADR-0020 and ADR-0021

This ADR amends ADR-0020's "remote provider kinds and any egress stay
structurally refused" clause and ADR-0021's "local socket, no egress, no
credentials" requirement, **only** for the `remote-openai-compatible`
kind under every gate above. Every other part of both ADRs stands. In
particular:

- flowerlyd refuses every remote kind unless started with a valid
  `--remote-providers` policy, which nothing installs by default;
- no image or package ships the provider unit or the policy; the
  templates are operator-installed;
- egress from any other process remains refused.

## Consequences

- One new provider kind with a reviewed, testable egress surface. All other
  processes keep zero egress.
- A new HTTP/TLS dependency set in providerd only: `reqwest` without
  default features, with `rustls`, `ring` and the `rustls-native-certs`
  system roots. Licences are within `deny.toml`
  (Apache-2.0, ISC, MIT); the `webpki-roots` bundle (CDLA) is excluded.
  `cargo-deny` and `cargo-audit` gate it.
- A schema migration (attempts table, nullable usage, `usage_source`) and
  new IPC commands (`inference.result`, the consent grant and revoke
  commands, `provider.submit` / `poll` / `cancel`) with the authorization
  inventory updated.
- Live behaviour is claimed only from recorded owner-run evidence with a
  dedicated least-privilege key (#263 PR E).

## Owner decisions (2026-10-30)

- **Q1 — approved:** dedicated least-privilege `flowerly-provider`
  account, without access to `state.db` or unrelated credentials.
- **Q2 — conditional:** private egress denied by default. Any exception
  must be explicitly approved, restricted to the configured destination,
  and protected against DNS rebinding. The operator's host is public, so
  none is configured.
- **Q3 — approved with conditions:** consent is per authenticated user,
  provider and destination; at most 30 days; immediately revocable;
  invalidated by a change of provider or destination; checked before any
  prompt leaves the machine.
- **Q4 — no default raw retention:** keep idempotency and accounting
  metadata only. Replay is optional and short-lived (implemented as
  memory-only). Honest typed results are returned without resubmission.

## Acceptance (2026-10-30)

The owner accepted this ADR with the Q1–Q4 decisions above as binding
conditions, and authorized integrating #264–#267 into `release/2` once
their required checks pass. Acceptance does **not** authorize:

- enabling remote egress on any host, or deploying the provider unit;
- contacting the operator's server or sending any real prompt;
- shipping the unit or policy in an image or package.

Each of those needs a separate explicit owner approval. The first live
request is the owner-run acceptance procedure of #263 PR E, with a
dedicated least-privilege key installed by the operator as an encrypted
systemd credential. Any private-address exception (Q2) also needs its own
approval. The destination's DNS must be re-verified as public before
deployment.

### Original questions

Kept as asked, for the record. The answers above supersede them.

- **Q1:** Should the provider run under a separate account
  (`flowerly-provider`, proposed), or as `flowerly` with path sandboxing only?
- **Q2:** Does the operator's server resolve to private addresses,
  requiring `allow_private_addresses` for that provider only?
- **Q3:** For consent grants, what scope (provider + destination host,
  proposed) and default expiry (proposed: 30 days, revocable any time)?
- **Q4:** Is retaining model output for the 30-day replay window
  acceptable for remote requests, or should remote responses keep only a
  digest and the usage row?

## Socket access architecture (accepted 2026-10-30, architecture only)

The owner explicitly accepted the socket access architecture for the
first remote provider, with the limitations documented at `044bdac`
([Remote Inference Operations](../remote-inference-operations.md#socket-access-model)).
The decision is recorded in PR #268. The accepted scope:

- **flowerlyd** is the trusted boundary for user identity, consent,
  administrator policy and profile authorization.
- **providerd** admits peers by kernel-attested process identity
  (`SO_PEERCRED` plus `SO_PEERGROUPS`) and the dedicated socket group
  `flowerly-provider-clients`.
- Socket-group membership grants **provider-level authority**: it can
  make the provider spend the remote key. It is **not** user consent.
- Ordinary users must never join the provider socket group.
- Root remains trusted within the documented threat model.
- Real Arch Linux/KVM security-boundary validation is required before
  operational acceptance.

This is **architecture acceptance only**. It does not accept:
- the OS security boundary;
- the provider unit's confinement;
- any operational use, release or deployment.

It does not authorize:
- merging #268;
- live API calls;
- installing or deploying the provider unit;
- enabling remote inference;
- changes to `master`, tags or releases.

The OS boundary stays unaccepted until the
[Remote Provider Unit Test Plan](../remote-provider-unit-test-plan.md)
has run in a real Arch Linux/KVM guest and its results have been
reviewed. Each of the other steps needs its own explicit owner approval.

## Acceptance tests

The tests below run offline against a local mock OpenAI-compatible server
(#266, #267). None of them needs the real server or a key.

- **Happy path:** discovery and inference succeed.
- **Model checks:** a missing model and a mismatched response model.
- **Server errors:** 401, 403, 429, 5xx.
- **Connection failures:** connection refused, connect and read timeouts,
  a TLS failure (untrusted certificate, wrong hostname), redirect refusal.
- **Bad responses:** an oversized body, malformed JSON, tool calls.
- **Limits:** prompt and output limits.
- **Accounting:** missing `usage` is recorded as unknown.
- **Restart:** provider restart with jobs in flight produces unknown
  outcomes.
- **Idempotency:** duplicate operation identities, and no resubmission
  of an unknown attempt.
- **Gates:** consent granted, refused and revoked; a `local-only` profile
  refused; flowerlyd with remote providers disabled refused.
- **No leaks or authority:** no network egress from flowerlyd or the echo
  provider; the sentinel key is absent from all errors, logs, IPC replies
  and database rows; model output grants no authority.
