# 0.0.4 completion plan and session handoff

Status: **0.0.4 OPEN, not release-certified.** Living document; update it in
the same PR that changes a workstream's state. Last verified: 2026-10-25 on
`release/2` at `14d6a2a` (PRs #207–#221 merged; `master` is an ancestor).

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
| 2 | Security alert backlog | #187 | Release-artifact fix and per-alert triage merged (#212, [triage](security-alert-triage.md)). No code defects; 0 Dependabot, 0 secret scanning. | **Maintainer:** decide on the remaining alerts and `project-sync*.yml` top-level permissions. No dismissals without that decision. |
| 3 | Dynamic counts and readiness | #31 Gate B | **Done**: merged #209 (one-snapshot counts; `ready` only when the DB answers). | — |
| 4 | Verified stale-socket recovery | #31 Gate B | **Done**: merged #210 (instance `flock` plus verified stale removal; PTY SIGKILL test requires it). | — |
| 5 | Machine-readable errors everywhere | #31, #53 | No store-backed command returns `internal` or an untyped client error (#213). Typed arguments for Flow/Goal (#216), Session metadata (#217) and `provider.register` (#218). | #53: audit the Compute families (`model.*`, `profile.*`, `workload.*`, `usage.record`, `inference.*`, `device.budget`) for silent coercion. |
| 6 | CLI priority contract | #31 Gate A | **Done**: `check-cli-contracts.sh` covers omitted, explicit and invalid priority (re-verified 2026-10-24). | — |
| 7 | Administrative authorization | #31 | Linux group enforcement merged (#222; cross-UID CI test green). See [command authorization](command-authorization.md). | Exact-SHA Arch VM evidence for the unit and groups. Per-user data ownership between `flowerly-users` members is not implemented. |
| 8 | PTY descendant containment | #72, #50 | Delegated-cgroup path merged; fail-closed without delegation. | Exact-SHA Arch VM positive test, adversarial migration/PID-reuse proof. |
| 9 | PTY streaming / attachment | #73 | **Done**: long-poll transport and races (#215), writer-lease expiry (#219), PTY limited to the service account (#222). Owner accepted ADR-0016 on 2026-10-25; #73 closed. | — |
| 10 | PTY crash/restart truth | #74, #63 | Fail-closed reconciliation merged. | sessiond SIGKILL / spawn-race matrix; spawn idempotency policy. |
| 11 | Arch release VM stability | #184 | OOM / disconnect under investigation. | Capture OOM evidence as artifacts; size runner. Never weaken acceptance. |
| 12 | Release evidence | #8, #31, #55 | Branch rules active. | Exact-SHA full Arch VM run on the promotion candidate; final owner review. |
| 13 | Per-user data ownership | #225 (P0) | Cross-user read and mutation reproduced with two real UIDs. Mitigation: the shipped unit is service-account-only (#225 PR 1). Design and plan approved on #225. PR 2 (#228, merged) records ownership (migration 0009, explicit `Caller`) but does **not** enforce it. `flowerlyd` refuses to start with a users group configured until #225 closes (owner question from #228, decided fail-closed; #229). PR 3 (#230, merged) enforces ownership for Flows, Goals, Sessions and Executions and adds the per-owner foreground (migration 0010, `scripts/test-user-ownership.py --cross-uid`). PR 4 (#232, merged) filters provenance by both endpoints, scopes all Status counts and keeps Generation `reason_goal` private (migration 0011). PR 5 scopes Compute: private profiles with per-owner names and read-only `system` profiles, private workloads and usage (migration 0012). PR 6 makes Generation candidates private and adds the audited Admin `generation.promote` with the append-only `admin_audit` table (migration 0013). No User command is pending; the startup refusal now also waits for `MULTI_USER_ADMISSION_RESTORED` (PR 8). | PR 7: audited Admin `ownership.inspect/claim/release`. PR 8: restore `flowerly-users` admission after full cross-UID acceptance. **0.0.4 is not release-ready while multi-user access is exposed.** |

Items 8–12 need the self-hosted Arch VM runner and owner review; do not mark
them done from unit tests or hosted CI alone.

## Non-negotiables

- Never weaken root-shell refusal, delegated-cgroup requirements, PTY
  cleanup, Arch guest acceptance or exact-commit promotion gates to get green.
- Never skip, disable or quarantine a test.
- Do not add RFC 0004 / #64 future work to 0.0.4 scope.
