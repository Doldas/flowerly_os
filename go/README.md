# Go

Go (Go 1.27+) is the specified language for future *distributed /
orchestration* services (see `adr/0002-go-for-distributed-orchestration.md`).
No Go code is implemented yet, so `go.work` is intentionally absent until a
module exists.

Subdirectories (when created):

- `remote/` — orchestration services that coordinate worker hosts
- `sync/` — replication and sync engines for distributed state

Never add Go to a security-critical path that Rust should own; introduce new
Go code only after the relevant design is agreed.