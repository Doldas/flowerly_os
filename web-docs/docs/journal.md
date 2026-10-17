# Journal

Chronological engineering notes. This is useful context for both humans and
future agents: what was built, what failed, and what comes next.

## 2026-10-11 — Repository scaffolded

Initialized the repository with `git init -b main`, configured the `origin`
remote for `flowerly_os`, and set `main` as the default branch.

## 2026-10-11 — Cargo workspace

Created the Rust workspace (resolver 2) with four shared crates and nine
binaries in total: the `agentic-*` libraries, the eight daemons, and the
`agenticctl` control plane. MSRV is Rust 1.80 and the license is
GPL-3.0-or-later.

## 2026-10-11 — `agentic-types`

Added typed, prefixed, UUID-backed identifiers (`FlowId`, `GoalId`, `AgentId`,
`SessionId`, `GenerationId`, `ExperimentId`, `ExecutionId`, and others) with
`FromStr` prefix validation and transparent serialization. Added the durable
domain models (`Flow`, `Goal`, `Session`, `Execution`, `Generation`,
`ProvenanceEdge`, `Capability`, `Artifact`) and their state enums.

## 2026-10-11 — `agentic-storage`

Implemented the SQLite-backed `Store` with an ordered, idempotent migration
runner that records applied versions in `schema_migrations`. The initial schema
(`migrations/0001_init.sql`) covers flows, projects, goals, agents, sessions,
executions, generations, observations, findings, experiments, decisions,
artifacts, provenance edges, and the user-model categories. Added CRUD and
round-trip tests.

## 2026-10-11 — `agentic-ipc`

Implemented Unix-domain-socket IPC with a 4-byte little-endian length prefix and
a UTF-8 JSON body, capped at 8 MiB. The versioned `Envelope` (protocol v1)
carries a correlation id and a tagged `Payload` (`ping`, `status`, `shutdown`,
`command`, `ok`, `err`). Added a server, a client, and a request/response test.

## 2026-10-11 — `agentic-core`

Built the shared daemon foundations: `ServiceConfig` with TOML loading and
environment overrides, `init_tracing` for text or JSON structured logs, and
`run_service`, which answers `ping`/`status`/`shutdown` and dispatches commands
to a service handler. Services exit cleanly on SIGTERM or an IPC shutdown.

## 2026-10-11 — Services and control plane

Wrote `agenticd`, `flowd`, `sessiond`, `goald`, `provenanced`, `capabilityd`,
`generationd`, and `evolutiond`. `flowd` bootstraps a default General Flow and
implements its lifecycle commands; the other services implement their initial
command sets; `capabilityd` and `evolutiond` expose status and a stub catalog.
Wrote `agenticctl` as the thin control-plane client.

## 2026-10-11 — Build and smoke test green

`cargo build`, `cargo test`, and `cargo clippy` pass. An end-to-end smoke test
starts `flowd`, queries it through `agenticctl status flowd`, creates and lists
Flows (`General` plus a new one), and shuts down cleanly.

## 2026-10-11 — Documentation, ADRs and RFCs

Added the ten root documents (README, VISION, ARCHITECTURE, ROADMAP,
CONTRIBUTING, SECURITY, CODE_OF_CONDUCT, DECISIONS, JOURNAL, CHANGELOG), the
thirteen `docs/` design documents, ADRs 0001–0009, and the RFC framework
(`rfcs/README.md`, `0000-template.md`). Added explanatory READMEs for the
scaffolded `go/`, `python/`, `c/`, `image/`, `packaging/`, `kernel/`, `shell/`,
`schemas/`, `tests/`, `evolution/` and `scripts/` directories.

## 2026-10-11 — Build system and CI

Installed `just` 1.58 and wired `justfile` + `Makefile` (`fmt`, `fmt-check`,
`lint`, `check`, `build`, `test`, `verify`, `run/serve/stop/status`; `image`,
`vm`, `vm-test` and `release` documented and enforced as placeholders where
unimplemented). `scripts/dev-runtime.sh` starts/stops/reports all daemons
against a dev runtime directory. Added GPL-3.0 LICENSE, `deny.toml`, and
GitHub Actions for ci, security (cargo-audit + cargo-deny), codeql, release,
dependency-review and scorecard, plus issue/PR templates, CODEOWNERS, and
dependabot.

## 2026-10-11 — Pushed to GitHub, pipeline green

Pushed `main` to `github.com/Doldas/flowerly_os`, made `main` the default
branch (removed the orphaned `master`). `scripts/bootstrap-github.sh` created
the issue labels (37), the ten milestones (0.0.1 → 0.1.0), the "Flowerly OS"
Projects v2 board, and four initial roadmap issues. First CI run surfaced and
fixed three integration problems: cargo-deny action repo did not exist
(replaced with `taiki-e/install-action`), `deny.toml` used keys removed by
cargo-deny 0.20 (fixed and verified locally), and `scorecard-action` needed a
concrete `v2.4.4` tag. All four checks are now green. Tagged `v0.0.1`, which
triggered the release workflow and published the first GitHub release with all
nine binaries plus SHA256SUMS.

## Upcoming — Release hardening

Planned: enable branch protection on `main` (require status checks and
conventional commit reviewers), add a `dependency-review` PR gate, and turn
cargo-deny advisories from informational into a required check.

## Upcoming — Sprint 2: Persistent State

Planned: complete migration-backed CRUD, provenance relationship queries, and
the full `agenticctl` admin surface. Not yet started.

## Upcoming — Sprint 3: Flows and Persistent Sessions

Planned: PTY-backed persistent sessions with attach, detach, and reattach, and
the Flow activation lifecycle end to end. Not yet started.

## Upcoming — Sprint 4: System Generations

Planned: generation manifests, candidate creation, activation, and rollback.
Not yet started.

## 2026-10-11 — Architecture alignment hardening

Reviewed the implemented repository against the Agentic OS grounding
specifications before persistent PTY work. The review found the overall direction
sound but identified four foundations that were cheap to correct now and
expensive later.

Implemented on issue #17 / PR #18:

- replaced per-daemon physical SQLite files with one durable local `state.db`
  while preserving logical service/domain ownership;
- separated ephemeral runtime sockets from durable state through
  `AGENTIC_STATE_DIR`;
- made migrations safe for concurrently starting daemons;
- split Session runtime state from ATTACHED/DETACHED presentation state;
- made provenance `ObjectRef` values validated and introduced explicit
  `actor:<name>` references;
- reserved `IntentId` and RFC 0001 so transient Intent is not conflated with a
  persistent Goal;
- recorded that capabilities complement Linux authority rather than replacing
  normal Linux execution;
- recorded ADR-0013: conceptual domains do not automatically become daemons;
- recorded RFC 0002 to keep Flow identity/config/runtime/presentation/policy
  concerns separable as Flows grow;
- merged the latest `master` ancestry into the active release work so release
  history becomes monotonic again when the PR lands.

The older journal entry referring to `main` reflects an earlier bootstrap
state. The repository's current release policy uses `master` as the protected
promotion branch and `release/N` as active integration branches.


## 2026-10-11 — Recovered abandoned core-model alignment branch

Reviewed the stale `arch/align-core-models` branch after PR #18 landed. The
branch was an interrupted precursor to the architecture-alignment work rather
than an independent feature line. Its main goals — shared durable state,
Session runtime/presentation separation, and typed provenance references — had
already been implemented more completely by issue #17 / PR #18.

The stale branch was deliberately **not** merged because it was far behind the
active `release/2` line and would have reintroduced older partial
implementations. Issue #20 instead finishes the remaining intent on a clean
branch from current `release/2`:

- malformed Flow IDs now fail at service boundaries instead of disappearing
  into optional values;
- Session creation validates that referenced Flows exist and range-checks PIDs;
- Goal creation range-checks priorities rather than silently defaulting;
- child Goal creation validates the parent before writing state;
- tests lock in these failure semantics.

This establishes a core invariant for future agent-facing APIs: typed domain
data must remain typed and validated when it crosses JSON/IPC boundaries.


## 2026-10-11 — Arch userspace substrate selected

Accepted ADR-0015: Flowerly OS uses Arch Linux as its initial userspace/package
substrate while retaining ADR-0004's upstream Linux kernel policy.

The decision intentionally does not turn Stable World into a conventional
rolling Arch installation. Arch repository changes become candidate-generation
inputs; Flowerly resolves and records an exact package set, tests the candidate
in Evolution World, and promotes or rejects the generation.

Added the first `image/arch/` scaffold with a minimal package list, expected
Flowerly binaries, a declarative mkosi rootfs prototype and CI validation. The
scaffold is explicitly not presented as a bootable image yet. Bootable disk
output, Flowerly packages, initramfs/UKIs, exact package artifact retention and
KVM boot tests remain roadmap work.


## 2026-10-11 — Nightly real-Arch kernel validation

Added issue #24 and a heavyweight system-validation workflow for the selected
Arch userspace direction.

The scheduled workflow resolves `master` plus the highest-numbered
`release/N` branch, fingerprints the code/build/image inputs, and skips the
expensive VM path if that fingerprint already passed. When it runs, the
self-hosted runner boots the official Arch cloud image under QEMU/KVM (with TCG
fallback), verifies the published image SHA256, performs a full `pacman -Syu`,
reboots into the installed Arch kernel, and executes the entire Flowerly
build/test/runtime smoke suite inside the guest.

The same workflow is manually dispatchable with a force option so it can serve
as final pre-release evidence. It is an Evolution World precursor rather than
the final generation-image test: the guest is still an upstream Arch image, not
yet a Flowerly generation image.


## 2026-10-12 — Persistent State milestone completed

Closed the 0.0.2 Core Runtime Skeleton tracking issue after the merged daemon,
migration, IPC and whole-runtime smoke coverage satisfied its exit criteria.

Completed the remaining 0.0.3 Persistent State work on issue #7:

- Execution and Generation IPC inputs now preserve typed-ID semantics instead
  of silently dropping malformed references;
- integer Execution fields reject invalid or overflowing input instead of
  coercing values;
- Execution writes can commit their causal provenance in the same SQLite
  transaction;
- Generation creation validates parent/Goal references, records causal
  provenance, and preserves typed Experiment references;
- generation activation supersedes the previous Stable generation and promotes
  the requested generation atomically;
- provenance is queryable in both directions;
- `agenticctl` exposes the corresponding durable-state administration inputs;
- a file-backed storage test reopens `state.db` and proves Flow, Goal, Session,
  Execution, Generation and provenance identities survive together;
- corruption tests now cover persisted Execution and Generation references.

With durable state complete, the next implementation milestone is 0.0.4:
PTY-backed persistent Sessions with detach/reattach semantics.


## 2026-10-12 — Review-driven release implementation plan

Converted the independent technical review of `release/2` at merge commit
`0d008f9e3764e5781aba9e0d2ab5678c63108278` into a durable, release-by-release
implementation plan.

The project now explicitly completes one release before starting implementation
of the next. Future review findings are preserved in
`docs/review-implementation-plan-2026-10-12.md`, while issue #8 and #31 own the
active 0.0.4 checklist.

0.0.4 now has two hardening gates before persistent PTY ownership: repository
trust/current-user-path correctness, then runtime safety. The first tranche
starts by removing PR-metadata shell interpolation, making promotion rules
coherent, moving pull-request builds off the persistent self-hosted runner,
repairing the Goal default-priority contract and agenticd self-test, executing
all daemon self-tests in CI, and fixing host-vs-guest HOME expansion in the Arch
VM harness.

Later release requirements are recorded but intentionally not implemented yet:
0.0.5 gains a minimal bootable candidate fixture and stronger Generation
semantics; 0.0.6 consumes that fixture in Evolution World; 0.0.7 focuses on Goal
execution semantics; 0.0.8 requires authenticated/bounded authority before
agents; and 0.0.9 becomes the distribution-grade image milestone.

## 2026-10-17 — RFC 0004 local compute tranches 1–3 prototype

Owner approved turning RFC 0004 / issue #64 into implementable prototype
tranches. Implemented as flowerlyd modules per ADR-0013 and the fresh,
accepted ADR-0020: the model catalog records only measured evidence
(migration 0006), expert TOML profiles are credential-free and append-only
versioned with acyclic depth-bounded inheritance (0007), and workload
admission + the usage ledger (0008) refuse GPU/accelerator backends and
unmeasured models fail-closed, gate on PSI memory pressure, and queue per
concurrency ceilings. All three tranches were written by the agent in one
Gauntlet session, black-box accepted on the real supervisor, and stay
preference/accounting data that grants no OS authority; remote kinds and
egress remain structurally refused, and the runtime-facing client protocol,
multi-device reattach and enforcement-grade scheduling stay gated exactly
as the RFC demands.
