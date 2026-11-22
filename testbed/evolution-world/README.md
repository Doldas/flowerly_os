# Evolution World testbed

The pre-existing Arch kernel acceptance scripts live in `arch-kernel/`.

The Evolution Lab guest probe (ADR-0024 D2, #411) lives here: `lab-probe.py`,
its unit in `systemd/`, its test-only Admin account in `sysusers/`, and in
`fixtures/` the console report that the host parser's tests read. Only
`scripts/prepare-lab-probe-rootfs.sh ROOTFS --test-profile` installs it, into
an A/B test image. The KVM harness that boots it is `evolutiond lab`; see
`web-docs/docs/evolution-lab.md`.

`just vm-test` (#412) builds a lab candidate with
`scripts/build-lab-candidate.sh` and boots it. `fixtures/lab-plan-initial.json`
(the unrecorded first boot of slot A) and `fixtures/lab-plan-rollback.json` are
the plans `scripts/lab-candidate.py prepare` writes for its test fixture,
which the host's plan tests parse.

No privileged host fallback is allowed.
