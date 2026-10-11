# packaging

Distribution and service packaging.

- `systemd/` — systemd unit files for every daemon
  (`agenticd.service`, `flowd.service`, `sessiond.service`, `goald.service`,
  `generationd.service`, `provenanced.service`, `capabilityd.service`,
  `evolutiond.service`) — see `adr/0006-systemd-service-supervision.md`
- `distro/` — distro integration (release build, dependency archives)
- `repository/` — package repository layout and signing

Not yet shipped; the daemons currently run from a Cargo build via
`scripts/dev-runtime.sh`.