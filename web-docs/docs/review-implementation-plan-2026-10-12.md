# Review-driven implementation plan — 2026-10-12

This document converts the technical review of `release/2` at commit
`0d008f9e3764e5781aba9e0d2ab5678c63108278` into an implementation order.

The project deliberately executes this plan **one release at a time**. Later
release requirements are recorded here so they are not lost, but implementation
must not jump ahead while the current release gate is incomplete.

## Governing rule

```text
finish current release invariants
        ↓
prove them with black-box tests
        ↓
merge and close the release tracking issue
        ↓
only then start the next release
```

Architecture documents describe the intended system. Release gates below define
what must be true in code before that architecture is trusted.

## Priority meaning

- **P0** — immediate security/correctness blocker.
- **P1** — required before the current subsystem receives greater authority.
- **P2** — near-term integrity, reproducibility or maintainability hardening.

## 0.0.4 — Flows and Sessions — ACTIVE

0.0.4 is not allowed to own real PTYs or processes until Gate A and Gate B are
complete.

### Gate A — repository trust and existing user paths

1. **P0: safe pull-request metadata handling**
   - Never interpolate PR body or branch names directly into shell source.
   - Treat PR metadata strictly as data.
   - Test quotes, newlines and shell metacharacters.

2. **P0: coherent branch promotion policy**
   - `feature/*`, `bugfix/*`, `docs/*` → active `release/N`.
   - `release/N` → `master`.
   - Other combinations are rejected.

3. **P0: isolate untrusted pull-request builds**
   - Pull-request Rust/docs jobs run on disposable GitHub-hosted runners.
   - Persistent `flowerly-runner` is reserved for trusted push, nightly,
     KVM and release work.

4. **P1: repair current CLI/server contracts**
   - `agenticctl goal create <objective>` works without `--priority`.
   - Missing and JSON `null` optional values have a documented meaning.
   - Invalid types and numeric overflow remain hard errors.

5. **P1: executable self-tests are real tests**
   - Repair `agenticd --self-test` foreign-key fixtures.
   - CI runs every daemon that advertises `--self-test`.

6. **P1: Arch final-validation path uses guest paths**
   - Never expand the host `$HOME` in commands intended for the guest.
   - Prove the VM runs the exact intended source commit.

### Gate B implementation note — socket/PID ownership tranche

The runtime ownership PR makes Unix-socket binding fail closed if the pathname
already exists (including a stale socket). Operators must stop the process,
verify the old instance is dead, and explicitly remove a stale socket before
restarting. **Automatic stale socket recovery is not yet implemented** because
a connectivity probe followed by unlink would race with a concurrent binder.
This remains an open Gate B criterion. The server only unlinks its own socket
inode; a replacement path is left alone.

The dev script records per-runtime PID and Linux process start ticks and checks
the daemon name and `AGENTIC_RUNTIME_DIR` before signaling. It no longer uses
host-global process name searches. For production use, a supervisor with
stronger process handles (pidfd/systemd) is still preferred.

Server-side IPC limits, SQLite worker isolation and trusted peer-credential
context remain separate Gate B requirements. Do not mark Gate B complete on
the strength of this tranche alone.

### Gate B implementation note — same-UID kernel peer boundary

The peer-identity tranche requires a Unix peer's kernel-reported UID to match
the owning daemon's socket UID. This runs at accept time, before allocating a
connection handler. A peer-credential lookup failure is denied. It preserves
the current single-user deployment model and avoids trusting user-provided
identity fields for **admission**.

This is *not* full actor authorization: existing `created_by` fields remain
client-supplied labels rather than authenticated provenance, and per-command
authorization / cross-user delegation are not implemented. Before agent
execution or shared-user service deployment, propagate trusted peer context to
domain handlers and derive security actors from that context. Keep shutdown
and process-control operations inside the authenticated boundary.

### Gate B implementation note — trusted creator identity

The authenticated IPC handler receives kernel-supplied UID, GID and PID from
its accepted Unix peer, following the same-UID admission check. The shared
daemon runner replaces client-supplied `created_by` in JSON object command
arguments with `uid:<peer-uid>` before handing them to service handlers.
This makes creator fields and `created_by` provenance recorded through the
standard IPC path represent the authenticated OS user rather than arbitrary
untrusted text. Non-object command arguments are rejected.

The single-user trust model intentionally treats processes owned by the same
UID as one principal. Peer PID is diagnostic context, not a durable process
identity or delegation token. CLI-provided `created_by` is no longer an
authoritative actor on the daemon IPC path. Domain functions invoked directly
outside IPC must use explicitly trusted actor input.

**Still open:** per-command capability policy, process control authorization
beyond the same-UID admission baseline, delegated actors, and audit records
that distinguish administrative callers sharing the same UID.

### Gate B — runtime safety

1. **Socket ownership**
   - Do not unconditionally delete a configured socket path.
   - Reject an already-live service.
   - Reject unexpected file types.
   - Remove only a verified stale socket.
   - Cleanup only the socket owned by the current process instance.

2. **Runtime process ownership**
   - `dev-runtime.sh` records the PIDs it started for that runtime.
   - Stop/restart affects only those processes.
   - Two Flowerly development runtimes may coexist safely.
   - Readiness must ping services rather than trust socket existence.

3. **IPC resource bounds**
   - Bound open connections and in-flight requests.
   - Add read, write and request deadlines.
   - Preserve the existing frame-size bound.
   - Validate response correlation IDs.

4. **Blocking database isolation**
   - Synchronous rusqlite work must not occupy Tokio runtime workers
     indefinitely.
   - Use a bounded blocking execution strategy/worker queue rather than
     pretending SQLite is asynchronous.
   - A slow client or SQLite writer lock must not destroy liveness.

5. **Local authority baseline**
   - Explicit runtime/state/socket permissions.
   - Capture Unix peer PID/UID/GID where supported.
   - Security actor identity comes from trusted request context, not arbitrary
     client-provided `created_by`.
   - Administrative shutdown and future process-control operations are
     authorization-aware.
   - Full delegated capability semantics remain a later Agent Runtime concern.

6. **Dynamic service health**
   - Uptime is calculated per request.
   - Counts/status reflect current state.
   - Distinguish liveness from readiness and domain health.
   - Define bounded graceful shutdown.

7. **Error contract**
   - New/touched operations return a machine-readable error code plus human
     message.
   - Initial codes: `invalid_argument`, `not_found`, `conflict`,
     `permission_denied`, `unavailable`.

### Flow invariants

Before PTY work is considered complete:

- activate a Flow atomically;
- define foreground scope as one foreground Flow per user/state scope;
- demote the previous foreground Flow in the same transaction;
- update `last_active` and `revision` atomically;
- define General as a stable system Flow with an explicit archive rule;
- reject ambiguous name lookup instead of selecting an arbitrary row;
- cover concurrent activation.

### Persistent PTY runtime

Only after the two gates:

- real PTY master ownership;
- child shell/process-group identity;
- runtime state independent from presentation state;
- attach / detach / reattach;
- bounded output buffer;
- terminal resize;
- process exit observation;
- safe signal/termination semantics;
- Flow association.

#### 0.0.4 release proof

A black-box integration test must execute the real control path:

```text
create Flow A
→ start shell
→ start long-running process
→ detach
→ activate Flow B
→ return to Flow A
→ reattach
→ same SessionId
→ same process still alive
```

It must also verify foreground exclusivity, durable metadata and authenticated
runtime ownership.

## 0.0.5 — Generations — RECORDED, DO NOT START YET

The current metadata skeleton becomes a real generation contract.

Required when 0.0.5 starts:

- typed, reproducible Generation manifest;
- exact Arch package versions/architecture/artifact hashes;
- repository acquisition/snapshot identity;
- Flowerly source/output hashes;
- upstream kernel source/config/artifact identity;
- explicit parent and reason provenance;
- remove public state setters that bypass domain transitions;
- database-level protection for critical single-current-generation invariants;
- activation history with actor, reason and previous generation;
- distinguish build/test/promotion intent from the actually booted generation;
- rollback target and history survive reboot;
- schema-version guard rejects databases newer than the binary understands;
- coherent SQLite backup/integrity/recovery workflow.

### Minimal bootable candidate fixture moves into 0.0.5

0.0.6 requires something bootable, while the distribution-grade image remains
0.0.9. To avoid a circular dependency, 0.0.5 must create a **minimal bootable
candidate fixture**:

```text
minimal Arch rootfs
+ Flowerly binaries
+ enough kernel/initramfs/boot plumbing for KVM
+ exact Generation manifest
```

This fixture is test infrastructure, not the final distribution image.

## 0.0.6 — Evolution Lab — RECORDED, DO NOT START YET

Evolution World consumes the candidate fixture created by 0.0.5:

```text
Generation manifest
→ build exact candidate
→ boot in KVM
→ verify manifest
→ health/integration tests
→ Experiment record
→ evidence
→ promote or reject
→ rollback test
```

The existing Arch-cloud nightly harness is a useful precursor, not a Flowerly
Generation test.

Compatibility and reproducibility are separate lanes:

- **upstream compatibility:** latest Arch/latest supported stable toolchain,
  rerun periodically even when Flowerly source is unchanged;
- **reproducibility:** pinned release toolchain, locked dependencies and exact
  package/image inputs.

## 0.0.7 — Goal Runtime — RECORDED, DO NOT START YET

Persistent Goal storage already exists. 0.0.7 therefore means **Goal execution
semantics**, not reimplementing CRUD:

- legal state transitions;
- decomposition and parent/child semantics;
- constraints and success evidence;
- dependency/scheduling semantics;
- links to Executions, Experiments and Generations;
- evidence-driven completion.

## 0.0.8 — Agent Runtime — RECORDED, DO NOT START YET

Entry criteria become stricter:

- Evolution World works;
- Goal Runtime works;
- authenticated IPC context exists;
- real capability/delegation policy exists;
- bounded execution exists.

Agent work then adds provider-neutral reasoning and tools. Linux remains the
authority/enforcement substrate. Every meaningful action records request,
decision, Execution, result and provenance.

## 0.0.9 — Minimal Agentic Environment — RECORDED, DO NOT START YET

0.0.9 is the first **distribution-grade** Flowerly environment, not the first
time anything boots. It owns:

- reproducible Arch-based system image;
- Flowerly packaging;
- systemd units;
- RuntimeDirectory/StateDirectory ownership;
- service users where appropriate;
- restart/watchdog policy;
- capability/filesystem hardening;
- journald;
- networking and users/login;
- recovery path;
- installer/image UX;
- exact package/kernel/build provenance.

## 0.1.0 — release evidence

0.1.0 must publish evidence with the release:

- release-specific notes;
- build manifest;
- tested source commit;
- toolchain/target/dependency identities;
- artifact hashes;
- test summary;
- boot/rollback/provenance results;
- one bounded agent-made change through the same Evolution pipeline.

## Cross-cutting backlog retained from the review

These items are deliberately recorded even when they are not current-release
blockers:

- preserve already-valid typed references in future data migrations;
- define which provenance endpoints must exist locally versus explicit external
  references;
- pagination/filtering/COUNT queries before lists can approach IPC frame limits;
- declare and test MSRV separately from current stable;
- use `--locked` for controlled/release builds;
- pin the release toolchain;
- eventually pin third-party Actions to reviewed commit SHAs;
- add migration immutability/checksum policy;
- use coherent backup for active WAL databases;
- redact or avoid logging sensitive free-text objectives/reasons.

## Implementation discipline

For each release:

1. update its tracking issue from this document;
2. create narrowly scoped implementation PRs;
3. attach a concrete automated acceptance test to each invariant;
4. merge only into the active `release/N` branch;
5. close the release issue only after its black-box exit test passes;
6. promote `release/N` to `master`;
7. cut the next release branch and only then activate the next section above.

This document is the durable review response. The roadmap remains the public
release summary; tracking issues contain the current detailed checklist.
