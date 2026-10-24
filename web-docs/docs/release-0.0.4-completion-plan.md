# 0.0.4 completion plan and session handoff

Status: **0.0.4 OPEN, not release-certified.** Living document; update it in
the same PR that changes a workstream's state. Last verified: 2026-10-24 on
`release/2` at `9556763` (PR #207 merged).

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
| 1 | Typed storage error provenance | #62, #56 | Classification audit merged (#207). | Stable per-request elapsed / lock-wait diagnostics. |
| 2 | Security alert backlog | #187 | 9 open Scorecard alerts, 0 Dependabot, 0 secret scanning (2026-10-24). | Least-privilege `permissions:` in `release.yml`, `project-sync*.yml`; document repo-posture alerts (BranchProtection, CodeReview, Maintained, Fuzzing, CII, SecurityPolicy). |
| 3 | Dynamic counts and readiness | #31 Gate B | Flow/Session/Goal live counts exist. | Remaining services report live counts and DB readiness. |
| 4 | Verified stale-socket recovery | #31 Gate B | Fails closed, manual recovery. | Prove a socket is stale (no listener, owned inode) before removal; never remove an active socket. |
| 5 | Machine-readable errors everywhere | #31, #53 | Flow, Goal, Session, Generation typed; storage unified. | Type argument errors in remaining legacy fallback handlers. |
| 6 | CLI priority contract | #31 Gate A | Omitted / valid priority covered. | Real CLI test for invalid priority. |
| 7 | Administrative authorization | #31 | Not started. | Design per-command admin authorization and audit semantics (needs owner decision). |
| 8 | PTY descendant containment | #72, #50 | Delegated-cgroup path merged; fail-closed without delegation. | Exact-SHA Arch VM positive test, adversarial migration/PID-reuse proof. |
| 9 | PTY streaming / attachment | #73 | Owner tokens, bounded replay merged. | Decide subscriber transport; slow-reader backpressure and race tests. |
| 10 | PTY crash/restart truth | #74, #63 | Fail-closed reconciliation merged. | sessiond SIGKILL / spawn-race matrix; spawn idempotency policy. |
| 11 | Arch release VM stability | #184 | OOM / disconnect under investigation. | Capture OOM evidence as artifacts; size runner. Never weaken acceptance. |
| 12 | Release evidence | #8, #31, #55 | Branch rules active. | Exact-SHA full Arch VM run on the promotion candidate; final owner review. |

Items 8–12 need the self-hosted Arch VM runner and owner review; do not mark
them done from unit tests or hosted CI alone.

## Non-negotiables

- Never weaken root-shell refusal, delegated-cgroup requirements, PTY
  cleanup, Arch guest acceptance or exact-commit promotion gates to get green.
- Never skip, disable or quarantine a test.
- Do not add RFC 0004 / #64 future work to 0.0.4 scope.
