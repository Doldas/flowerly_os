# schemas

Versionable message/repository schemas that span more than one component.

Long-lived wire and storage schemas belong in the owning crate
(`agentic-ipc` for the envelope protocol, `agentic-storage` for SQLite
migrations). This directory hosts shared cross-component schemas (for
example, the model-invocation contract) when they stabilize.