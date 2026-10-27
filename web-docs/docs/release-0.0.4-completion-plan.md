# 0.0.4 completion plan and session handoff

Status: **0.0.4 OPEN, not release-certified.** Living document; update it in
the same PR that changes a workstream's state. Last verified: 2026-10-27 on
`release/2` after PRs #207–#246 (`master` is an ancestor). Owner NO-GO of
2026-10-27: #72, #74 and #184 are release blockers; #206 needs explicit approval.

Authoritative trackers: roadmap [#8](https://github.com/Doldas/flowerly_os/issues/8),
hardening gates [#31](https://github.com/Doldas/flowerly_os/issues/31).
Issues and code win over this page; re-verify branch SHA, PR state and CI
before acting (see `AGENTS.md`).

## How work lands

1. Branch from current `release/2` (`fix/<issue>-…`, `feat/<issue>-…`, `docs/…`).
2. Run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
   `cargo test --workspace` and `bash scripts/preflight-docs.sh` **from a clean
   checkout** (a leftover `target/flowerly-arch-rootfs/` makes the rootfs fixture
   refuse to run; use a scratch `git worktree`).
3. Open a PR into `release/2`, link the issue, wait for exact-head `rust`,
   `docs`, CodeQL and security checks. The owner has authorized squash-merging
   green, scoped implementation PRs into `release/2`.
4. Merged changes accumulate in the standing **`release/2` → `master` promotion
   PR** (currently #206). The owner merges that PR manually when 0.0.4 is ready;
   `release/2` is kept afterwards for continued development. Never auto-merge
   master.
5. Comment on the affected issue with merged SHA, evidence and what remains,
   and update the workstream table below.

## Workstreams

| # | Workstream | Issue(s) | State | Next concrete step |
| --- | --- | --- | --- | --- |
| 1 | Typed storage error provenance | #62, #56 | **Done**: classifier (#207), stage-aware deadline errors and per-request timing (#211); #62 closed. | — |
| 2 | Security alert backlog | #187 | Per-alert evidence, impact and disposition in [triage](security-alert-triage.md); no dismissals. #243: least-privilege workflow tokens and the CI policy check `test-workflow-permissions.py` (alerts 92 and 93 close after promotion). | **Maintainer:** branch-ruleset bypass actors and deletion/non-fast-forward rules for `release/*` (alert 1); keep alert 2 open until `SECURITY.md` is on `master`. |
| 3 | Dynamic counts and readiness | #31 Gate B | **Done**: merged #209 (one-snapshot counts; `ready` only when the DB answers). | — |
| 4 | Verified stale-socket recovery | #31 Gate B | **Done**: merged #210 (instance `flock` plus verified stale removal; PTY SIGKILL test requires it). | — |
| 5 | Machine-readable errors everywhere | #31, #53 | **Done**: #53 closed (#205, #213, #216–#218, #223); #31 item ticked with evidence on 2026-10-27. | — |
| 6 | CLI priority contract | #31 Gate A | **Done**: `check-cli-contracts.sh` covers omitted, explicit and invalid priority (re-verified 2026-10-24). | — |
| 7 | Administrative authorization | #31 | **Done**: #222, #227, #234, #235; `test-admin-authorization.py --cross-uid` passes in the Arch guest; #31 item ticked with evidence. | — |
| 8 | PTY descendant containment | #72, #50 | #246: the adversarial suite `test-pty-containment-adversarial.py` (namespace escape, exact PID reuse, exhaustion, orphans and zombies, fail closed) passes on the real `flowerlyd` in the Arch guest on `0f1beeb`; no defect found. Limits are in [containment](pty-descendant-containment.md): the shipped unit keeps PTY spawning disabled. | Close #72 after the release candidate's exact-SHA Arch run includes the suite. |
| 9 | PTY streaming / attachment | #73 | **Done**: long-poll transport and races (#215), writer-lease expiry (#219), PTY limited to the service account (#222). Owner accepted ADR-0016 on 2026-10-25; #73 closed. | — |
| 10 | PTY crash/restart truth | #74, #63 | #244: crash matrix `test-pty-daemon-recovery.py` passes in the Arch guest; two defects fixed (unkeyed spawn records, `management-*` leak); one `session.spawn` idempotency policy in [IPC mutation outcomes](ipc-mutation-outcomes.md). | Close #74 after the release candidate's exact-SHA Arch run includes the matrix. |
| 11 | Arch release VM stability | #184 | #245: `failure-classification.txt` and `host-resource-start.txt` on every run; `exit_status` capture fixed; measured runner sizing and the unexplained 2026-10-26 host reset documented in [resource timeline](arch-vm-resource-timeline.md). | Repeated exact-SHA passes on the release candidate. |
| 12 | Release evidence | #8, #31, #55 | #55 closed; ruleset covers `release/*`. #247: newer-schema refusal, plus the verified upgrade (schema 8 → 13) and rollback by restoring a backup in [releases](releases.md#upgrade-and-rollback-003-004). | Exact-SHA full Arch VM run on the promotion candidate; final readiness report; owner GO. |
| 13 | Per-user data ownership | #225 (P0) | **Done**: PRs 1–8 (#226 through #236) enforce ownership for every domain and add audited Admin promotion and ownership commands; #225 closed 2026-10-26 with Arch guest evidence. The shipped default is service-account-only; multi-user admission is the opt-in drop-in `/usr/share/flowerly/systemd/flowerlyd-multi-user.conf`. See [command authorization](command-authorization.md). | — |
| 14 | Database path override | #237 | **Done**: #238 unsets `AGENTIC_DATABASE_PATH` in the unit and the drop-in, and logs a warning when the path leaves the state directory. | — |
| 15 | PTY isolation override | #240 | **Done**: #241 unsets `FLOWERLY_PTY_ISOLATION` and `FLOWERLY_PTY_CGROUP_ROOT` in the unit and the drop-in; `flowerlyd` warns in development isolation. | — |
| 16 | Dependency updates | #231 | **Done**: tokio 1.54.1 ported in #242 (lockfile v3 kept); #231 closed without merge (it targeted `master`). | — |

Items 8–12 need the self-hosted Arch VM runner and owner review; do not mark
them done from unit tests or hosted CI alone.

## Non-negotiables

- Never weaken root-shell refusal, delegated-cgroup requirements, PTY
  cleanup, Arch guest acceptance or exact-commit promotion gates to get green.
- Never skip, disable or quarantine a test.
- Do not add RFC 0004 / #64 future work to 0.0.4 scope.
