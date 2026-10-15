# Implementation decision register and dependency plan

**Status:** Proposed work sequencing, not ADR acceptance. For owner review before code begins. Scope: #82–#95; release blockers #50/#72–#74/#79 remain independent.

## Decisions to explicitly approve

| ID | Proposed decision | Open condition | Owner action |
| --- | --- | --- | --- |
| D1 | Keep ADR-0011 shared `state.db` until proven cutover | Existing core foreign keys and provenance | Approve no premature physical split |
| D2 | Domain-typed command/query traits, not arbitrary SQL facade | Exact API details and use-site audit | Review #83 contract |
| D3 | Two axes: InstallationMode and MemoryBackend | Invalid combinations / fallback | Approve policy before #85 |
| D4 | CoreOnly disables memory/AI without erasing stored data | Actual boot dependencies | Review #88 test matrix |
| D5 | Existing IPC peer-credential and PTY guardian authority stay trusted | #50/#72-#74 unfinished | Approve temporary adapter exceptions |
| D6 | Host authority requires owner-bound handles and verified kernel effect | Broker/delegation design | Security review #95 before #92 mutation |
| D7 | Cluster is explicit authenticated memory-only feature | LibSQL capability and threat model | Separate #87 acceptance |
| D8 | Proposed ADR-0017/0018 remain Proposed until explicit decision | Full contract and rollback proof | Mark Accepted **only** after explicit approval |

Approving this docs PR should not itself be interpreted as authorization to accept ADRs or change production behavior.

## Dependency roadmap

1. **Document review:** #91 and #82 inventory and this suite; record owner decisions on D1–D8. Keep release PTY gates unchanged.
2. **#83 first code slice:** add typed core repository traits plus tests and an adapter delegating to existing single-file `Store`. Avoid a second DB, new daemon and host privilege movement.
3. **#94 storage contract tests:** exercise same SQL adapter against real SQLite and mock failure cases; preserve existing API/compatibility and error types. Add #93 focused negative import fixtures only when exception list is stable.
4. **#85 config policy + #88 CoreOnly:** implement validated mode/backends and lifecycle behind feature gates, without physical split or network surprise. Arch boot evidence mandatory.
5. **#86 local memory:** isolated SQLite schema/index contract with retention/authorization and zero core-boot dependence.
6. **#84 cutover:** only after a separately reviewed and experimentally proven crash-consistent state machine, table-by-table ownership and rollback proof. Block if any cross-file atomic invariant is unresolved.
7. **#92/#95 Linux authority extraction:** only after real delegated #72 and #74, preserving guardian/IPC identity. No blanket refactor or unverifiable mount success.
8. **#87 Cluster:** late opt-in, real backend concurrency and hostile-peer tests; never core replication.
9. **#89 final cross-mode acceptance:** measured performance, operations, backup/recovery, security and exact-SHA Arch/KVM evidence.

## Definition of ready for each code PR

Issue link and one bounded feature, typed public contract, backwards-compatible data strategy, explicit unsafe/authority review, negative tests, real adapter acceptance where relevant, no gate weakening and no merge without owner authorization. Split #83, #84, #92 and #87 into separate draft PRs.

## Rollback/escalation

If a mock passes but real kernel/SQLite fails: stop that feature. If a migration can lose receipts/provenance: do not start cutover. If cgroup delegation or writer identity is uncertain: reject PTY spawn. If Cluster auth/consistency uncertain: never start peer listeners. Preserve old backups and make operator recovery explicit. Any acceptance claim must cite its exact commit and run; docs alone do not meet release gate.

## Status ledger

**Proposed:** these interface and migration decisions; ADR-0017/0018. **Implemented:** existing shared SQLite `Store`, trusted IPC and PTY machinery (without claiming security acceptance). **Tested:** only scenarios evidenced by exact workflow run. **Accepted:** ADR-0011 and separately accepted existing ADRs. No work on #55 is authorized by this plan.
