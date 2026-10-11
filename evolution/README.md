# evolution

The Evolution World: where aggressive experiments happen without risking the
stable system (see `docs/EVOLUTION.md` and `adr/0007-kvm-evolution-world.md`).

- `vm/` — QEMU/KVM harness (build → boot → health check → test → shutdown)
- `benchmarks/` — benchmark definitions and runners
- `fitness/` — fitness functions used to judge candidates
- `promotion/` — promotion/rejection decision handling (Stable-World promotion is explicitly NOT automatic)

Not yet implemented; `evolutiond` runs as a stub. The future workflow is
exposed as `just vm-test`.