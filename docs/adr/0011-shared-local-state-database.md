# 0011: One Shared Local SQLite State Database

Date: 2026-10-11  
Status: Accepted  
Supersedes: the per-service database topology in ADR-0005

## Context

Flows, Goals, Sessions, Executions, Experiments, Generations and provenance form
one causal system. The initial implementation gave each daemon its own SQLite
file. That kept process boundaries visually clean, but it created the wrong
failure model for a single-machine operating-system core:

```text
flowd commits Flow
goald commits Goal
provenanced fails
=> partially committed system state
```

It also made foreign keys across domains ineffective and would force ordinary
local consistency problems into distributed transactions before Flowerly OS has
any evidence that it needs physically separate databases.

Runtime sockets are also ephemeral and should not determine where durable state
lives.

## Decision

A local Flowerly OS installation uses one physical SQLite database,
`state.db`, by default.

Logical ownership remains separated by service and domain. Physical co-location
does **not** mean every service may freely bypass domain APIs.

Configuration separates:

```text
runtime_dir  ephemeral sockets / runtime files
state_dir    durable local state
state.db     shared structured state database
```

Environment overrides:

```text
AGENTIC_RUNTIME_DIR
AGENTIC_STATE_DIR
AGENTIC_DATABASE_PATH   explicit recovery/test override
```

For an unprivileged development installation, state follows XDG state
conventions. A future system installation may configure `state_dir` to
`/var/lib/flowerly`.

SQLite remains configured with foreign keys, WAL and a busy timeout. Migrations
use an IMMEDIATE transaction and re-check the migration version inside that
transaction so concurrently starting daemons converge safely on one schema.

## Consequences

- Cross-domain relationships live in one ACID database.
- Foreign keys can represent real local relationships.
- Startup no longer creates one copy of the complete schema per daemon.
- Cross-domain transactions are possible when a future invariant genuinely
  requires them.
- Service boundaries remain logical/API boundaries, not database-file
  boundaries.
- If scaling or fault-isolation evidence later justifies splitting stores, that
  becomes an explicit migration rather than an assumption made during bootstrap.
- Pre-0.1 per-service development databases are not automatically merged.
  `AGENTIC_DATABASE_PATH` remains available for inspection/recovery.
