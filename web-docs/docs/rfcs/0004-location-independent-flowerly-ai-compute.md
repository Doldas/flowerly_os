# RFC 0004: Location-independent Flowerly and AI compute profiles

Status: **Proposed vision / architecture exploration; partial local prototype implemented under ADR-0020 — see the implementation-status addendum at the bottom. NOT an accepted capability and no release commitment**  
Date: 2026-10-13  
Scope: post-0.0.4 architecture; do not bypass active runtime, identity, security or release gates

## Product thesis

Flowerly OS should allow humans and agents to continue work across device boundaries, without requiring the display device, Flowerly runtime, and AI inference engine to occupy the same machine. A local laptop can host all three; a tablet/browser can be a client of a remote Flowerly workstation or VM; an organization can centralize inference or entire managed runtimes. **Local-first and standalone remain supported, not second-class modes.**

Three independent placement decisions:

1. **Client / presentation:** native workstation, laptop, phone, tablet, thin client, browser.
2. **Flowerly runtime:** local Linux installation, dedicated workstation, isolated VM, on-premises server or provider-hosted Linux instance. Owns Flow/Goal/Session state, authorization, PTYs/processes, provenance and policy enforcement.
3. **Inference execution:** model running locally alongside runtime, another user-owned machine, organizational inference cluster or explicitly permitted external provider API (e.g. OpenAI). The inference layer cannot substitute for OS authority.

A browser is a client interface, **not itself a VM**. A VM is one possible runtime isolation/deployment mechanism.

## Foundational invariants

- OS boot, recovery and administration **must not depend on AI models, cloud connectivity or a particular provider**.
- Flow identity, authorized Session ownership and process lifetime belong to Flowerly runtime, not a transient client or model endpoint. Switching devices does not start a second shell or impersonate an existing attachment.
- Model providers are interchangeable behind declared capability and API contracts. A bigger model is not inherently better: selection uses task suitability, benchmark evidence, latency, cost and hardware fit.
- Data locality and network egress are explicit policy. No silent upload of files, prompts, secrets, source code or private Flow context to remote model APIs; users and administrators must see destinations and approve policy.
- The user decides the **ceiling** of permitted AI resource use. Hardware decides feasibility. An organization's policy may further restrict, but must not quietly grant permissions.
- Model selection or custom profiles **never authorize** filesystem, terminal, network, administrative or cross-tenant actions. OS capabilities remain separately enforced.
- No fabricated seamless migration: reconnect to the **same surviving runtime** is distinct from failover, VM migration or recovering a dead supervisor.
- Agents and clients are replaceable; durable provenance, work, decisions and state remain in the system. Avoid storing only conversational history as the source of truth.

## Human-facing preferences

Avoid naming the principal AI profiles after battery or power-management modes.

Suggested initial UX vocabulary (subject to usability testing):

- **Essential:** lightweight everyday help, minimal interference with foreground applications.
- **Enhanced:** capable general work, coding, planning and analysis when supported.
- **Advanced:** prefer strongest suitable compatible models for demanding workloads, within explicit resource ceilings.
- **Custom:** named, user-authored profiles, for expert users.

These labels describe **AI experience/capability preference**, not an absolute benchmark score or a guarantee of quality. Separate **Background Activity** (quiet / normal / intensive) from model capability; one high-end coding request must not imply unlimited background agents. CPU, RAM and VRAM constraints should be enforceable where supported and honestly labeled otherwise. Present estimated hardware demand and incompatibilities before activation.

Expert profiles should permit create/clone/rename/edit/import/export/version/share, with schema validation and a human-readable format (e.g. TOML), explicit inheritance, and no included credentials. Per-agent role and task policy may specify:
- Allowed model IDs, versions, quantizations, context limits, modality and compatibility.
- Minimum guaranteed capability or approved fallback; never silently downgrade when a task requires a particular feature or verified safety property.
- Preferred provider/locality; local-only, explicit remote opt-in, or organizational approved providers.
- Memory/VRAM limits, CPU quotas, accelerator preference, concurrent inference limits, scheduling priority and bounded background activity.
- Fallback behavior: queue, ask, use a compatible smaller model, or fail clearly; cost/token ceilings for remote APIs.

Profile minimum/maximum values must reflect real hardware and policies. Report feasibility; do not assume all GPU backends support enforceable per-process VRAM quotas. Memory pressure, OOM recovery, model unloading, preemption, thermal/battery constraints and interactive QoS need empirical Linux tests.

## Organization-managed deployments

Potential deployment shapes:
- **Personal local:** runtime and local model on a user's computer; no mandatory server.
- **Personal remote:** phone/tablet/browser connects to a user's stationary workstation; inference local to that workstation or approved API.
- **Managed workstation:** per-user Flowerly runtime, centralized model registry/policy, optionally shared inference.
- **Thin client / VDI:** authenticated browser/native client connects to an organization-managed Flowerly VM or server runtime.
- **Team inference:** multiple isolated Flowerly runtimes access a shared organizational model-serving fleet; neither shared context nor shared OS authority is implied.

Require clear organization policy hierarchy (org upper bounds -> team/workstation defaults -> user preferences -> per-agent/task restrictions), with the most restrictive effective grants, explicit exceptions and audit logs. Consider offline operation, enrollment, revocation, updates, quotas, accounting/showback, identity federation, tenant isolation and deployment inventory. Enterprise support/security/compliance is a distinct product and verification effort, not something a profile setting confers.

## Interfaces and boundaries to investigate

1. **Runtime-facing client protocol:** authenticated device/session attachment, discovery, reconnection, replay/flow control, cancellation, resumable display state, protocol versioning, revocation and trusted input. Remote connections require encrypted transport; do not expose local Unix sockets directly to untrusted networks.
2. **Inference provider boundary:** capability discovery, provider/model identity, request ID, token/latency/cost accounting, data routing rules, rate limits, retries, timeouts, structured results and provenance. API failures must not corrupt Flow/Session state. Credential storage is separate from portable profiles.
3. **Model catalog / compatibility checker:** actual measured memory footprint, VRAM/CPU backend, architecture, context limits, accepted modalities, licensing/trust and performance evidence. No hardcoded assumption that one model covers every agent role.
4. **AI resource scheduler:** admit/queue/stop inference workloads according to device budgets, priorities and real OS pressure; isolate per-user and per-tenant costs; integrate Linux cgroups and GPU-specific controls only where verified.
5. **Remote runtime and fleet management:** distinct from inference serving. Identity, encryption, patching, backups, provisioning, audit, emergency local administration and fail-closed revocation require separate security review.

## Threat model and correctness risks

Untrusted remote input, credential/token theft, stale/replayed client attachment, session ID guessing, cross-user/cross-tenant access, malicious provider response or model output, prompt injection, accidental cloud egress, denial of service from unbounded inference requests, budget bypass via custom profiles, remote client disconnection, provider outage and runtime host crash. Do not equate SO_PEERCRED for local Unix sockets with authentication over a network. No network-facing session protocol until authenticated authority boundaries and tests exist.

Test at minimum: local-only no-provider operation; hardware model incompatibility; constrained-memory admission/release; no-remote-egress policy; per-user profile permission ceilings; provider outage/fallback with explicit consent; two-device connect/disconnect/reattach to same live runtime and same child process; unauthorized device denial; bounded streaming; runtime crash/restart honest limitations; tenant separation where implemented.

## Roadmap relationship and decision gates

- **0.0.4 remains unchanged:** implement truthful production Linux PTY supervision, identity/authorization, persistent Flow/Session semantics, SQLite reliability, required release checks and executable acceptance tests (#8, #31, #50, #55, #56, #62, #63). Do not describe prototype PTYs as a deployed runtime.
- **Later design tranche:** decide which runtime/provider/client boundaries warrant separate processes under ADR-0013; write accepted ADRs only after prototypes; compare native client vs browser thin client; prove a single authenticated reconnect.
- **Later local-AI tranche:** model catalog, provider interface, resource profiles, scheduling and offline fallback with benchmarked capability compatibility.
- **Later enterprise tranche:** fleet policy, hosted/VM runtimes, centralized inference and tenant-safe remote access after audited security gates.
- **Do not commit to a release number** for these capabilities until current 0.0.4 blockers are closed and scope is reviewed.

## Open questions

- What should an idle laptop do when its Flowerly runtime lives elsewhere and connectivity vanishes?
- Which tasks require local inference versus approved external API, and who may change that?
- How to measure a model's **task capability** rather than treating parameter count as a quality scale?
- What are minimum resource guarantees per agent role, and what can be degraded safely?
- How should Flow/Session state be backed up without pretending a running PTY can be migrated?
- What happens when a shared inference server reaches capacity, policy is revoked mid-request, or an API times out after a side effect?
- How do organizational policies remain inspectable and user agency remain meaningful?

This RFC documents the **user's proposed direction**, not a delivered feature, committed commercial strategy or approved technical decision.

## Implementation status addendum (2026-10-17)

Three prototype tranches exist under the direction of this RFC, gated by
[ADR-0020](../adr/0020-ai-compute-layer-is-a-module.md) (accepted): the AI
compute layer lives inside flowerlyd as modules until enforcement evidence
exists. They implement the **local** sub-tranche of interface 2 (provider
registry boundary), interface 3 (catalog/compatibility) and part of
interface 4 (admission bookkeeping):

- **Tranche 1 — model catalog + measured device budget:** `models`
  (migration 0006), metric-honest compatibility verdicts
  (`UNKNOWN` with reasons instead of estimates), `/proc/meminfo`-only
  numbers, per-process VRAM explicitly `not-enforced`
  ([compute-catalog](../compute-catalog.md)).
- **Tranche 2 — expert profiles + provider registry:** credential-free
  TOML profiles, append-only versions, depth-bounded acyclic inheritance
  with only-tightening locality; providers restricted to `local-unix`,
  remote kinds refused `PermissionDenied` because no egress policy
  authorization exists ([compute-profiles](../compute-profiles.md)).
- **Tranche 3 — workload admission + usage ledger:** fail-closed admission
  against measured evidence and effective ceilings (GPU backends refused
  this tranche; missing evidence refused), PSI pressure guard, queueing
  per `concurrency_max`, durable decision/ceiling/reason snapshots and a
  token/latency/cost ledger ([compute-scheduling](../compute-scheduling.md)).
- **Tranche 4 — reference provider execution contract:** a fault-isolated
  `providerd` process (ADR-0021) with bounded synthetic echo generation,
  capability discovery, ADMITTED-only execution through flowerlyd,
  operation_id replay on the ledger row, and profile-fallback outage
  semantics (`ask`/`queue`/`fail` with explicit consent; `downgrade`
  refused) ([compute-inference](../compute-inference.md)).

What remains **gated and NOT implemented**: any execution of inference,
cgroup/GPU-enforced resource controls for inference, remote provider
policy authorization, the runtime-facing client protocol / multi-device
reattach, remote runtime & fleet management, organization policy
hierarchy, and every enterprise deployment shape. The RFC's own decision
gates and issue #64 constraints remain: no release-number commitment, no
claims of delivered capability, security and action authority enforced by
OS mechanisms rather than model or profile selection.
