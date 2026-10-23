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
