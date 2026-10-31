# Remote provider Arch/KVM acceptance (test only)

Inputs for `scripts/remote-provider-arch-kvm-acceptance.sh` (#263,
ADR-0022). **Prepared, never executed.** The plan, the checks and the
authorization a run needs are in
`web-docs/docs/remote-provider-unit-test-plan.md`.

| File | Purpose |
| --- | --- |
| `network.json` | Guest-internal addresses and the policy each must meet (checked against providerd's production policy by a Rust unit test) |
| `arch-image.json` | The pinned Arch cloud image (v20261015.611680), primary key and approved signing subkey; provenance and open trust assumptions in the plan's "Image provenance". Execution still needs its own authorization |
| `checks.json` | Every check the evidence must report, and whether it tests production code or the harness |
| `evidence.schema.json` | Evidence structure; `scripts/remote-provider-kvm-evidence.py` adds the rules |
| `probe-targets.json` | Paths and listeners for `scripts/provider-unit-probe.py` in the guest |
| `guest/guest-acceptance.py` | Guest-side runner; refuses outside this run's disposable KVM guest |
| `guest/mock-provider.py` | Offline HTTPS mock, scripted DNS and connection sinks |
| `guest/provider-client.py` | Socket client used under exact kernel identities |
| `guest/make-test-pki.sh` | Per-run throwaway PKI (no key is committed) |
| `guest/interpose.sh` | Guest-only power-cut interposer for lifecycle tests |

Nothing here holds a real key, a real destination or a real prompt.
