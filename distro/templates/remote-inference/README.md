# Remote inference templates (ADR-0022, #263)

**Not installed, not shipped, not enabled.** Nothing in the build, the
rootfs installer or the image reads this directory. Remote inference stays
disabled until an administrator deliberately installs these files and the
owner has approved live egress.

| File | Installs to | Purpose |
| --- | --- | --- |
| `flowerly-provider.sysusers.conf` | `/usr/lib/sysusers.d/` | `flowerly-provider` account (owner Q1) and the `flowerly-provider-clients` socket group |
| `flowerly-provider-remote@.service` | `/etc/systemd/system/` | one confined provider process per instance `%i` |
| `provider-service.toml` | `/etc/flowerly/providers/%i.service.toml` | socket name, runtime directory and client group |
| `provider-remote.toml` | `/etc/flowerly/providers/%i.toml` | destination, API style, model allowlist, limits |
| `flowerlyd-remote-inference.conf` | `/etc/systemd/system/flowerlyd.service.d/` | flowerlyd joins the client group and reads the gate policy |
| `remote-providers.toml` | `/etc/flowerly/remote-providers.toml` (root, 0644) | the flowerlyd gate (disabled when absent) |

The operator procedure (install, discovery-only preflight, single-prompt
acceptance, evidence, emergency disable and rollback) is
`scripts/remote-provider-operator.sh`, documented in
`web-docs/docs/remote-inference-operations.md`.

The API key never appears in any of these files. Install it as an
encrypted systemd credential by typing it at the prompt, never as a
command argument:

```sh
systemd-creds encrypt --name=<instance>.key - /etc/credstore.encrypted/<instance>.key
```

Rollback: stop and disable `flowerly-provider-remote@<instance>`, remove
the flowerlyd drop-in and `/etc/flowerly/remote-providers.toml`, then
restart flowerlyd. Remote inference is disabled again, and users' consent
grants no longer match any enabled provider.
