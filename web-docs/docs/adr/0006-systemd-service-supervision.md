# 0006: systemd as the Initial Service Supervisor

Date: 2026-10-11
Status: Accepted

## Context

Flowerly OS needs reliable supervision for its daemons: start ordering, restart
policy, logging, cgroup accounting, socket activation and dependency management.
Rebuilding any of these would be a large, risky distraction from the project's
actual goals, and Linux already provides them.

## Decision

Flowerly OS services are ordinary systemd services. Unit files are shipped in
`packaging/systemd/`. Services use systemd facilities instead of reimplementing
watchdog, restart, dependency, logging and cgroup behavior. The supervised
services are `agenticd`, `flowd`, `sessiond`, `goald`, `generationd`,
`provenanced`, `capabilityd` and `evolutiond`. Each daemon also runs and stops
cleanly as a normal foreground process, so it can be tested without systemd.
Daemon logs are structured and journald-compatible.

## Consequences

- Supervision, restart, ordering and log collection come for free.
- Units make the runtime topology explicit and reviewable.
- The project inherits systemd's operational model and its platform assumptions.
- Implemented today: daemons with a uniform status/shutdown contract and
  structured logging. Planned: the systemd unit files and socket activation.
