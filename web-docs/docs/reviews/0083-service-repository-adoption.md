# #83 service adoption: Session and Execution repository boundary

**Status: Implemented in this branch, pending exact-commit checks and owner acceptance.**

This PR moves **two production daemon clients**, `sessiond` and `agenticd`, from direct `Store` Session/Execution persistence calls to the existing typed `CoreCommandRepository` and `CoreQueryRepository` APIs. It also adds typed collection queries to the same interface. It does not move the SQLite driver, copy state, introduce a new backend or change the database format.

## Call-site inventory

| Service / command | Before | After | What remains unchanged |
| --- | --- | --- | --- |
| sessiond / session.create | `insert_session_with_provenance` | `create_session(CreateSession)` | One IMMEDIATE transaction across Session + CreatedBy edge |
| sessiond / session.create + operation_id | `insert_session_with_provenance_once` | `create_session_once` | Actor binding, replay identity, stored response, receipt and mutation in one transaction |
| sessiond / session.terminate validation/read | `get_session` | `CoreQueryRepository::session` | Existing non-PTY guard; stale guardian process management unaffected |
| sessiond / session.list + status count | `list_sessions` | `CoreQueryRepository::sessions` | Existing response structure and status behavior |
| agenticd / execution.create | `insert_execution_with_provenance` | `create_execution(CreateExecution)` | Optional goal/agent references and causal-edge transaction |
| agenticd / execution.show + execution.list | `get_execution` / `list_executions` | `CoreQueryRepository::execution/executions` | JSON responses, absent record behavior, authorization boundary |

**Deliberate exclusions:** `sessiond`'s actual Linux PTY guardian, `/proc`, cgroup delegation, orphan recovery, mount and PID identity checks. Session terminal state update methods and test-only direct `Store` setup remain unchanged, as does agenticd's test-only Store setup. `Store` still owns every SQL connection; no service receives a raw connection.

## Negative and regression checks to run on exact PR SHA

1. `just fmt-check` — mandatory, and do not approve until actually passed.
2. `just lint` and `just test` — run full workspace checks including existing PTY tests; no release gate skips.
3. sessiond: same operation_id + same actor/request returns original response and one Session; same key + changed payload fails without duplicate; durable provenance edge remains.
4. agenticd: execution create/query/list preserve typed ID, response format, optional references, and existing causality (goal or agent) edges.
5. Existing invalid Session PID, unknown Flow and malformed references must retain rejection behavior.
6. Run positive and adversarial cgroup/PTY Arch acceptance independently of this service-storage refactor; a passing storage test is *not* PTY acceptance.

## Deferred follow-ups

- Review and, if justified, expose a typed `SessionLifecycleRepository` for durable state/presentation transitions, without granting host authority or splitting process reconciliation from its security proof.
- Move remaining storage reads/writes incrementally after import inventories (#93) and per-command owner approval; preserve raw trusted Linux IPC/kernel boundary adapters (#91/#95).
- Physical dual-core migration (#84) remains blocked on its own crash consistency, ownership and rollback gates. #55 is out of scope.

**Evidence status:** Proposed policy; implemented call-site migrations and regression test source; no claim that CI is Tested or that ADR-0017/0018 is Accepted until supported by actual run and owner decision.
