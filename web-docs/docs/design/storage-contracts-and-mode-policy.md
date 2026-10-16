# Storage contracts and installation-mode policy

**Status:** Proposed implementation specification, not an accepted ADR or functioning code. **Issues:** #82 #83 #85 #86 #87 #88. Accepted ADR-0011 and proposed ADR-0017/0018 govern unresolved questions.

## Domain boundaries

* The current physical authority is the one-file SQLite `state.db`. Keep `Store` transactions, triggers, foreign keys, migration sequencing and actor-derived provenance intact until a separately approved migration.
* CoreState owns Flow, Goal, Session, Generation, activation, causal provenance and the existing idempotency receipt. The existing agents/executions/experiments and other cross-linked tables stay in core pending a per-table ownership decision.
* Memory owns optional authorized histories, user memory, scripts and embeddings only after an explicit inventory and consent/retention design. A speculative `user_facts` move is not approved.
* Cross-domain writes must not be redefined as atomic if routed to separate files, processes or replicated storage. Prefer keeping every safety-critical invariant within a single local core transaction.

## Rust interface design (#83)

Use narrow, object-safe domain capabilities with typed request/response and application-level errors, not arbitrary SQL:
- `CoreCommandRepository`: commands such as `create_flow_with_provenance`, `create_goal_with_provenance`, `create_session_with_provenance`, `activate_flow`, `activate_generation`; enforce actor-bound idempotency inside the same transaction as the mutation.
- `CoreQueryRepository`: typed reads and provenance traversal with explicit authorized scopes.
- `MemoryRepository`: authorized insert/search/delete, retention/erase and pagination; must be unavailable without disabling core boot.
- `EmbeddingIndex`: validated `model_id`, `dimension`, `version` and finite vector values, tenant restrictions, deterministic ties and rebuild semantics.
- `ReplicationController`: opt-in, memory-only, explicit peer identity, schema capability negotiation and conflict reporting; no core-state mutation.
- `CoreUnitOfWork` should be an internal implementation detail or command-scoped transaction boundary: never publish a generic `commit` API allowing separate provenance/idempotency commits.

Proposed error families: NotFound, Invalid, Unauthorized, Conflict, Busy/Locked, Unavailable, Corrupt, Unsupported and UnknownCommitOutcome. Do not collapse Busy or permission failures into success. Cancellation/timeouts may leave a commit outcome unknown; use an actor-scoped idempotency key and reconciliation rather than blind retries. No external database daemon is required.

## Two independent axes (not two competing mode enums)

| InstallationMode | MemoryBackend | Allowed | Boot and network |
| --- | --- | --- | --- |
| Standalone (default) | LocalSqlite | Proposed normal mode | Fully offline; zero memory network listeners |
| Standalone | Disabled | Explicit degraded opt-out only | Core boots; cognitive features report unavailable |
| Cluster | VerifiedReplicated | Explicitly enabled after security acceptance | Authenticated encrypted approved peers only |
| Cluster | LocalSqlite | Defined offline fallback **only with explicit safe policy**, never unsanctioned split-brain writes | Core always local |
| CoreOnly | Disabled | Required | No memory DB open, no AI provider requirement, no network listener |

Other combinations are invalid unless an explicit ADR amendment is accepted. Configuration in `/etc/flowerly/mode` is proposed, **not present**. Missing config should select Standalone/local only if permission/format validation and storage readiness are proven. Malformed or insecure config must never silently select Cluster; refuse unsafe features, preserve local recovery, and surface typed failure. Mode transitions must not delete disabled data. `AGENTIC_DATABASE_PATH` remains the current recovery/test override and cannot silently trigger physical migration.

## Cluster safety conditions

mDNS is discovery, never trust. Enrollment: verified operator authorization, authenticated identities, encrypted transport, revocation, key rotation, per-peer ACL and audit trails. Explicitly define write leadership, conflict resolution, replay resistance, partition policy, schema upgrades and deletion propagation before any implementation claim. Never promise generic multi-writer libSQL/Turso behavior without actual version-specific tests. Threat indicators are quarantined, versioned, signed and source-tagged; incoming data must not execute code or modify Stable World.

## Acceptance (future)

Unit/mock tests cover typed requests, rejects and mode validation. Real SQLite tests cover constraints, cross-connection atomicity, cancellation and disk-full. Actual Arch/KVM startup proves CoreOnly offline/corrupt memory behavior. Three-node real backend tests prove identity, partition, conflicts, reconnect, revocation and no core-state replication before Cluster acceptance. Record SHA, job URL and resulting evidence for each; passing a mock alone is never sufficient.
