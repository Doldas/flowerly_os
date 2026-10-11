# kernel

Kernel policy and packaging.

- `config/` — concrete kernel configuration for Flowerly OS images
- `packaging/` — kernel build/packaging integration

Policy per `adr/0004-upstream-linux-kernel-policy.md`: upstream Linux is the
substrate. No downstream kernel fork; configuration and packaging live here.