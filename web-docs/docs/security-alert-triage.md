# Security alert release review (0.0.4)

Related: [security acceptance #187](https://github.com/Doldas/flowerly_os/issues/187).

A successful CodeQL, cargo-audit or cargo-deny workflow does not prove
the GitHub Security alert backlog is empty. Neither this script nor an
empty alert inventory certifies the release.

## Maintainer workflow

On a trusted workstation, authenticate `gh` with permission to read
Code Scanning, Dependabot and Secret Scanning alerts.

```bash
umask 077
python3 scripts/security-alert-inventory.py Doldas/flowerly_os > security-alerts.private.json
python3 scripts/security-alert-triage.py security-alerts.private.json
```

The inventory is redacted but contains private security metadata. Do not
commit it, publish it as a CI artifact or paste raw secret values in issues.
Prefer storing it outside the repository; dispose of it following incident policy.

Triage emits only aggregate totals and priority buckets:

- Exit `0`: no open alerts **in this snapshot**; other release gates and
  freshness/authorization review remain required.
- Exit `1`: open alerts or API retrieval failure; release review blocked.
- Exit `2`: missing, incomplete, malformed or inconsistent inventory;
  release review blocked.

Any open secret-scanning alert requires immediate review without exposing
its contents, type or validity. Unknown severity is unresolved, not safe.
Missing API access is not evidence of zero alerts.

## Release acceptance evidence

1. Record exact release SHA, inventory timestamp, reviewer and authorized
   access to all three alert APIs.
2. Investigate alerts in GitHub Security, prioritize critical/high findings,
   and record ownership, exploitability, remediation PRs and negative tests.
3. For dismissals, record evidence, approver and compensating controls.
4. Verify exact-SHA CodeQL, cargo-audit, cargo-deny, PTY/Arch kernel
   acceptance and protected promotion before explicit maintainer sign-off.
5. Link findings and resolutions to #187, #31 and #8 without secrets.

The scripts neither dismiss alerts nor bypass release protection.

## Triage snapshot 2026-10-24 (`release/2`)

Open alerts: **9 Code Scanning (all OpenSSF Scorecard), 0 Dependabot,
0 Secret Scanning.** There are no open CodeQL, cargo-audit or cargo-deny
findings. None of the nine alerts is a code defect. **Dismissal needs
maintainer approval**; until a maintainer records it here and in #187,
each alert stays open.

| Alert | Rule | Location | Assessment | Proposed action |
| --- | --- | --- | --- | --- |
| 113 | TokenPermissions | `release.yml:75` | The `release` job needs `contents: write` to publish a GitHub release. The top-level default is `contents: read`. Only `v*` tag pushes run it, on the trusted runner. | Accept as required; dismiss as "used in tests / won't fix" with this rationale. |
| 93, 92 | TokenPermissions | `project-sync.yml`, `project-sync-manual.yml` | No top-level `permissions:` block, deliberately: 2026-10-18 probes showed some permissions/trigger combinations create empty failed push-validation runs. These workflows only touch the GitHub Project board. | Maintainer decision: accept with that evidence, or re-probe whether a top-level block is safe now. |
| 1 | BranchProtection | repository | Rulesets `MERGERULES` (24880732) and `FLOWERLY_RELEASE_REQUIRED_CHECKS` (25024564) are active (#55). Scorecard reads only the default-branch classic protection settings. | Verify ruleset coverage in settings, then dismiss with a link to #55. |
| 33 | CodeReview | repository | Solo-maintainer history. PRs are reviewed by the owner and CI, not by a second human. | Accept for the current project stage. |
| 34 | Maintained | repository | Scorecard heuristic based on project age and activity. | Re-evaluates on its own; no action. |
| 32 | Fuzzing | repository | No fuzz targets yet. The IPC frame decoder and SQLite row parsing are the best candidates. | Track as future hardening; not a 0.0.4 gate. |
| 35 | CIIBestPractices | repository | No OpenSSF Best Practices badge. | Optional; not a 0.0.4 gate. |
| 2 | SecurityPolicy | repository | `SECURITY.md` exists; Scorecard warned "no linked content". Private vulnerability reporting is enabled. | `SECURITY.md` now links the private advisory URL (this PR). The alert should close on the next Scorecard run. |
