# Expert AI profiles and the provider registry (RFC 0004 tranche 2)

Status: **prototype surface, second tranche of RFC 0004**; builds on the
[local model catalog](compute-catalog.md). No remote egress exists.

## Expert profiles

Profiles are portable, human-readable **TOML preference documents**,
stored append-only (`ai_profiles` + `ai_profile_versions`, migration 0007).
Key contract points, straight from RFC 0004:

- **Credential-free by construction**: keys like `api_key`, `token`,
  `password`, `secret` and any endpoint field are structurally refused with
  a dedicated invariant message. Unknown TOML keys are rejected
  (`deny_unknown_fields`), so the schema cannot drift silently.
- **Versioned**: `profile.edit` appends a new version; earlier versions stay
  exported verbatim (`profile.export`, `profile.version_history`). A profile
  identity never changes id; renames update the display name with a
  provenance edge.
- **Explicit inheritance** (`base = '<profile name>'`): resolution walks the
  chain by name using each profile's latest version, bounded to a depth of
  8, with cycle detection. Child explicit values win; a child **cannot
  widen** a parent's restriction — locality only ever tightens toward
  `local-only`, and `allowed_models` unions (never shrinks) the parent set.
- **Capability tiers** (`essential`/`enhanced`/`advanced`/`custom`) are
  experience labels, not benchmarks. **Background activity** is an
  independent field (`quiet`/`normal`/`intensive`): one high-end request
  never implies unlimited background agents.
- **Locality**: `local-only` (default), `approved-remote-opt-in`,
  `org-approved-providers` are recorded *preferences*. No transport
  authorizes anything today.
- **Fallback**: `queue`, `ask` (default), `downgrade`, `fail` — recorded
  behavior; silent downgrade never happens because no executor exists yet.
- Ceilings: `memory_max_bytes`, `vram_max_bytes`, `concurrency_max` are
  admission-advice data for the later scheduler tranche; VRAM is always
  labeled unenforced.

## Provider registry

`ai_providers` stores capability labels only. The registered kind is
restricted to **`local-unix`**; any remote kind (`openai-api`,
`org-cluster`, ...) is refused with **`PermissionDenied`** — remote egress
policy authorization does not exist, and fail-closed refusal is the honest
default. Endpoints may reference local runtime sockets only; credentials
have no home in this table.

## Command surface

`profile.create|clone|rename|edit|import|export|list|show|
version_history|resolve` and `provider.register|list|show`.

`profile.import` and `profile.create` honor `operation_id`
(unknown-outcome replay safety, #63 semantics). Name conflicts are typed
`Conflict`; unknown ids `NotFound`; malformed schema `InvalidArgument`.

## Security boundaries

- Profile and provider selection grant **no OS authority** (RFC 0004):
  no process, filesystem or network action exists in this tranche.
- No credentials, endpoints or tokens are storable in the profile or
  provider tables; both refuse them structurally.
- Everything stays local IPC with kernel-verified actor labels; no remote
  client path exists yet.

## Honest limitations

- No inference execution, scheduling or cost accrual yet (scheduler is the
  next gated tranche).
- `profile.resolve` reflects recorded preference data; it does not perform
  feasibility checks against the device budget yet.
- Registry kinds beyond `local-unix` are intentionally unreachable until
  the egress policy authorization gate exists.
