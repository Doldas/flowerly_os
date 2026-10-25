//! Typed, backend-agnostic core command/query seam (issue #83).
//!
//! Domain callers receive no SQL connection and cannot commit provenance
//! independently of the corresponding Flow/Goal creation. This is an
//! incremental adapter over the accepted single-file SQLite Store: it neither
//! creates a second backend nor changes existing service behavior.

use crate::types::{
    Caller, Execution, ExecutionId, Flow, FlowId, Goal, GoalId, GoalStatus, ObjectRef,
    ProvenanceEdge, Session, SessionId,
};

use super::{FlowView, MutationOperation, Result, Store};

/// A domain command, not a generic transaction or SQL request.
/// Provenance edges are part of the same durable SQLite transaction.
pub struct CreateFlow<'a> {
    pub flow: &'a Flow,
    pub provenance: &'a [ProvenanceEdge],
}

/// Goal creation with its causality evidence in one commit.
pub struct CreateGoal<'a> {
    pub goal: &'a Goal,
    pub provenance: &'a [ProvenanceEdge],
}

/// Session metadata creation is separate from PTY spawn or kernel authority.
/// The session record and its provenance are one SQLite transaction.
pub struct CreateSession<'a> {
    pub session: &'a Session,
    pub provenance: &'a [ProvenanceEdge],
}

/// Execution records and causal edges commit as one SQLite transaction.
pub struct CreateExecution<'a> {
    pub execution: &'a Execution,
    pub provenance: &'a [ProvenanceEdge],
}

/// Minimal core command capability. Extend command-by-command after each
/// transaction and authorization invariant has been reviewed.
///
/// Every operation on an existing object takes the kernel-attested caller
/// and is owner-scoped inside its transaction (#225); creates record the
/// owner carried by the new object and check its references in the same
/// transaction.
pub trait CoreCommandRepository {
    fn create_flow(&mut self, command: CreateFlow<'_>) -> Result<()>;
    /// A replay response and its durable mutation are one SQLite transaction.
    /// The authenticated actor must match the Flow's durable creator.
    fn create_flow_once(
        &mut self,
        command: CreateFlow<'_>,
        operation: &MutationOperation<'_>,
    ) -> Result<String>;
    fn create_goal(&mut self, command: CreateGoal<'_>) -> Result<()>;
    fn create_goal_once(
        &mut self,
        command: CreateGoal<'_>,
        operation: &MutationOperation<'_>,
    ) -> Result<String>;
    fn activate_flow(&mut self, caller: Caller, id: &FlowId) -> Result<FlowView>;
    fn archive_flow(&mut self, caller: Caller, id: &FlowId) -> Result<FlowView>;
    fn set_goal_status(&mut self, caller: Caller, id: &GoalId, status: GoalStatus) -> Result<()>;
    /// Returns the Session as it was before termination.
    fn terminate_session(&mut self, caller: Caller, id: &SessionId) -> Result<Session>;
    fn create_session(&mut self, command: CreateSession<'_>) -> Result<()>;
    fn create_session_once(
        &mut self,
        command: CreateSession<'_>,
        operation: &MutationOperation<'_>,
    ) -> Result<String>;
    fn create_execution(&mut self, command: CreateExecution<'_>) -> Result<()>;
}

/// Read-only, typed queries. No raw SQL or underlying driver handles escape.
/// Every read is owner-scoped to the kernel-attested caller (#225).
pub trait CoreQueryRepository {
    fn flow(&self, caller: Caller, id: &FlowId) -> Result<Option<FlowView>>;
    fn flows(&self, caller: Caller) -> Result<Vec<FlowView>>;
    fn foreground_flow(&self, caller: Caller) -> Result<FlowView>;
    fn goals(&self, caller: Caller) -> Result<Vec<Goal>>;
    fn goal(&self, caller: Caller, id: &GoalId) -> Result<Option<Goal>>;
    fn session(&self, caller: Caller, id: &SessionId) -> Result<Option<Session>>;
    fn sessions(&self, caller: Caller) -> Result<Vec<Session>>;
    fn execution(&self, caller: Caller, id: &ExecutionId) -> Result<Option<Execution>>;
    fn executions(&self, caller: Caller) -> Result<Vec<Execution>>;
    fn provenance_from(&self, from: &ObjectRef) -> Result<Vec<ProvenanceEdge>>;
}

impl CoreCommandRepository for Store {
    fn create_flow(&mut self, command: CreateFlow<'_>) -> Result<()> {
        self.insert_flow_with_provenance(command.flow, command.provenance)
    }

    fn create_flow_once(
        &mut self,
        command: CreateFlow<'_>,
        operation: &MutationOperation<'_>,
    ) -> Result<String> {
        self.insert_flow_with_provenance_once(command.flow, command.provenance, operation)
    }

    fn create_goal(&mut self, command: CreateGoal<'_>) -> Result<()> {
        self.insert_goal_with_provenance(command.goal, command.provenance)
    }

    fn create_goal_once(
        &mut self,
        command: CreateGoal<'_>,
        operation: &MutationOperation<'_>,
    ) -> Result<String> {
        self.insert_goal_with_provenance_once(command.goal, command.provenance, operation)
    }

    fn activate_flow(&mut self, caller: Caller, id: &FlowId) -> Result<FlowView> {
        self.activate_flow_for(caller, id)
    }

    fn archive_flow(&mut self, caller: Caller, id: &FlowId) -> Result<FlowView> {
        self.archive_flow_for(caller, id)
    }

    fn set_goal_status(&mut self, caller: Caller, id: &GoalId, status: GoalStatus) -> Result<()> {
        self.set_goal_status_for(caller, id, status)
    }

    fn terminate_session(&mut self, caller: Caller, id: &SessionId) -> Result<Session> {
        self.terminate_session_for(caller, id)
    }

    fn create_session(&mut self, command: CreateSession<'_>) -> Result<()> {
        self.insert_session_with_provenance(command.session, command.provenance)
    }

    fn create_session_once(
        &mut self,
        command: CreateSession<'_>,
        operation: &MutationOperation<'_>,
    ) -> Result<String> {
        self.insert_session_with_provenance_once(command.session, command.provenance, operation)
    }

    fn create_execution(&mut self, command: CreateExecution<'_>) -> Result<()> {
        self.insert_execution_with_provenance(command.execution, command.provenance)
    }
}

impl CoreQueryRepository for Store {
    fn flow(&self, caller: Caller, id: &FlowId) -> Result<Option<FlowView>> {
        self.flow_for(caller, id)
    }

    fn flows(&self, caller: Caller) -> Result<Vec<FlowView>> {
        self.flows_for(caller)
    }

    fn foreground_flow(&self, caller: Caller) -> Result<FlowView> {
        self.foreground_flow_for(caller)
    }

    fn goals(&self, caller: Caller) -> Result<Vec<Goal>> {
        self.goals_for(caller)
    }

    fn goal(&self, caller: Caller, id: &GoalId) -> Result<Option<Goal>> {
        self.goal_for(caller, id)
    }

    fn session(&self, caller: Caller, id: &SessionId) -> Result<Option<Session>> {
        self.session_for(caller, id)
    }

    fn sessions(&self, caller: Caller) -> Result<Vec<Session>> {
        self.sessions_for(caller)
    }

    fn execution(&self, caller: Caller, id: &ExecutionId) -> Result<Option<Execution>> {
        self.execution_for(caller, id)
    }

    fn executions(&self, caller: Caller) -> Result<Vec<Execution>> {
        self.executions_for(caller)
    }

    fn provenance_from(&self, from: &ObjectRef) -> Result<Vec<ProvenanceEdge>> {
        Store::provenance_from(self, from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ProvenanceRelation, Timestamp};

    fn create_through_trait(
        repo: &mut impl CoreCommandRepository,
        flow: &Flow,
        edge: &ProvenanceEdge,
    ) {
        repo.create_flow(CreateFlow {
            flow,
            provenance: std::slice::from_ref(edge),
        })
        .expect("typed create");
    }

    #[test]
    fn sqlite_adapter_persists_object_and_provenance() {
        let mut store = Store::open_in_memory().expect("open");
        store.migrate().expect("migrate");
        let flow = Flow::new("Typed facade", "user");
        let edge = ProvenanceEdge {
            from: ObjectRef::from(&flow.id),
            relation: ProvenanceRelation::CreatedBy,
            to: ObjectRef::actor("user").expect("actor"),
            created_at: Timestamp::now(),
            note: None,
        };

        create_through_trait(&mut store, &flow, &edge);

        let reader: &dyn CoreQueryRepository = &store;
        assert_eq!(store.get_flow(&flow.id).expect("query"), Some(flow.clone()));
        let edges = reader
            .provenance_from(&ObjectRef::from(&flow.id))
            .expect("edge");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].relation, edge.relation);
    }

    #[test]
    fn sqlite_failed_duplicate_command_keeps_first_provenance() {
        let mut store = Store::open_in_memory().expect("open");
        store.migrate().expect("migrate");
        let flow = Flow::new("Already present", "user");
        let edge = ProvenanceEdge {
            from: ObjectRef::from(&flow.id),
            relation: ProvenanceRelation::CreatedBy,
            to: ObjectRef::actor("user").expect("actor"),
            created_at: Timestamp::now(),
            note: None,
        };
        create_through_trait(&mut store, &flow, &edge);
        assert!(store
            .create_flow(CreateFlow {
                flow: &flow,
                provenance: &[],
            })
            .is_err());
        assert_eq!(
            CoreQueryRepository::provenance_from(&store, &ObjectRef::from(&flow.id))
                .expect("provenance")
                .len(),
            1
        );
    }

    #[test]
    fn typed_replay_preserves_original_result_and_provenance() {
        let mut store = Store::open_in_memory().expect("open");
        store.migrate().expect("migrate");
        let first = Flow::new("Once", "uid:1000");
        let edge = ProvenanceEdge {
            from: ObjectRef::from(&first.id),
            relation: ProvenanceRelation::CreatedBy,
            to: ObjectRef::actor("uid:1000").expect("actor"),
            created_at: Timestamp::now(),
            note: None,
        };
        let operation = MutationOperation {
            actor: "uid:1000",
            command: "flow.create",
            operation_id: "typed-test-1",
            request_payload: r#"{"name":"Once"}"#,
            response_payload: r#"{"result":"first"}"#,
        };
        let response = CoreCommandRepository::create_flow_once(
            &mut store,
            CreateFlow {
                flow: &first,
                provenance: std::slice::from_ref(&edge),
            },
            &operation,
        )
        .expect("first insert");
        assert_eq!(response, operation.response_payload);
        let second = Flow::new("Once", "uid:1000");
        let replay = CoreCommandRepository::create_flow_once(
            &mut store,
            CreateFlow {
                flow: &second,
                provenance: &[],
            },
            &operation,
        )
        .expect("replay");
        assert_eq!(replay, response);
        assert!(store.get_flow(&first.id).unwrap().is_some());
        assert!(store.get_flow(&second.id).unwrap().is_none());
        assert_eq!(
            CoreQueryRepository::provenance_from(&store, &ObjectRef::from(&first.id))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn typed_replay_rejects_untrusted_actor_without_effect() {
        let mut store = Store::open_in_memory().expect("open");
        store.migrate().expect("migrate");
        let flow = Flow::new("Protected", "uid:1000");
        let operation = MutationOperation {
            actor: "uid:9999",
            command: "flow.create",
            operation_id: "typed-test-denied",
            request_payload: "{}",
            response_payload: "{}",
        };
        assert!(CoreCommandRepository::create_flow_once(
            &mut store,
            CreateFlow {
                flow: &flow,
                provenance: &[],
            },
            &operation,
        )
        .is_err());
        assert!(unscoped_flow(&store, &flow.id).unwrap().is_none());
    }

    #[test]
    fn typed_session_and_execution_round_trip_with_provenance() {
        use crate::types::{
            ExecutionId, ProvenanceRelation, SessionPresentationState, SessionState,
        };

        let mut store = Store::open_in_memory().expect("open");
        store.migrate().expect("migrate");
        let session = Session {
            owner: crate::types::Owner::User(1000),
            id: SessionId::new(),
            flow_id: None,
            kind: "metadata-only".to_owned(),
            pid: None,
            cwd: None,
            state: SessionState::Running,
            presentation_state: SessionPresentationState::Detached,
            created_by: "uid:1000".to_owned(),
            created_at: Timestamp::now(),
        };
        let execution = Execution {
            owner: crate::types::Owner::User(1000),
            id: ExecutionId::new(),
            agent_id: None,
            goal_id: None,
            reason: Some("typed repository".to_owned()),
            command: None,
            exit_code: None,
            duration_ms: None,
            started_at: Timestamp::now(),
        };
        let edge = ProvenanceEdge {
            from: ObjectRef::from(&session.id),
            relation: ProvenanceRelation::CreatedBy,
            to: ObjectRef::actor("uid:1000").expect("actor"),
            created_at: Timestamp::now(),
            note: None,
        };
        CoreCommandRepository::create_session(
            &mut store,
            CreateSession {
                session: &session,
                provenance: std::slice::from_ref(&edge),
            },
        )
        .expect("create session");
        CoreCommandRepository::create_execution(
            &mut store,
            CreateExecution {
                execution: &execution,
                provenance: &[],
            },
        )
        .expect("create execution");
        assert_eq!(
            unscoped_session(&store, &session.id).unwrap(),
            Some(session.clone())
        );
        assert_eq!(
            unscoped_execution(&store, &execution.id).unwrap(),
            Some(execution)
        );
        assert_eq!(
            CoreQueryRepository::provenance_from(&store, &ObjectRef::from(&session.id))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn typed_session_idempotency_replay_does_not_duplicate_metadata() {
        use crate::types::{SessionPresentationState, SessionState};

        let mut store = Store::open_in_memory().expect("open");
        store.migrate().expect("migrate");
        let session = Session {
            owner: crate::types::Owner::User(1000),
            id: SessionId::new(),
            flow_id: None,
            kind: "metadata-only".to_owned(),
            pid: None,
            cwd: None,
            state: SessionState::Running,
            presentation_state: SessionPresentationState::Detached,
            created_by: "uid:1000".to_owned(),
            created_at: Timestamp::now(),
        };
        let operation = MutationOperation {
            actor: "uid:1000",
            command: "session.create",
            operation_id: "typed-session-1",
            request_payload: r#"{"kind":"metadata-only"}"#,
            response_payload: r#"{"result":"created"}"#,
        };
        let first = CoreCommandRepository::create_session_once(
            &mut store,
            CreateSession {
                session: &session,
                provenance: &[],
            },
            &operation,
        )
        .expect("initial session");
        let retry = Session {
            id: SessionId::new(),
            ..session.clone()
        };
        let replay = CoreCommandRepository::create_session_once(
            &mut store,
            CreateSession {
                session: &retry,
                provenance: &[],
            },
            &operation,
        )
        .expect("replay session");
        assert_eq!(first, replay);
        assert!(unscoped_session(&store, &session.id).unwrap().is_some());
        assert!(unscoped_session(&store, &retry.id).unwrap().is_none());
    }

    fn unscoped_flow(store: &Store, id: &FlowId) -> Result<Option<Flow>> {
        store.get_flow(id)
    }

    fn unscoped_session(store: &Store, id: &SessionId) -> Result<Option<Session>> {
        store.get_session(id)
    }

    fn unscoped_execution(store: &Store, id: &ExecutionId) -> Result<Option<Execution>> {
        store.get_execution(id)
    }

    struct RecordingRepository {
        creations: usize,
    }

    impl CoreCommandRepository for RecordingRepository {
        fn create_flow(&mut self, command: CreateFlow<'_>) -> Result<()> {
            assert_eq!(command.provenance.len(), 1);
            self.creations += 1;
            Ok(())
        }

        fn create_flow_once(
            &mut self,
            _command: CreateFlow<'_>,
            operation: &MutationOperation<'_>,
        ) -> Result<String> {
            Ok(operation.response_payload.to_owned())
        }

        fn create_goal_once(
            &mut self,
            _command: CreateGoal<'_>,
            operation: &MutationOperation<'_>,
        ) -> Result<String> {
            Ok(operation.response_payload.to_owned())
        }

        fn create_goal(&mut self, _command: CreateGoal<'_>) -> Result<()> {
            self.creations += 1;
            Ok(())
        }

        fn activate_flow(&mut self, _caller: Caller, id: &FlowId) -> Result<FlowView> {
            Err(super::super::StorageError::NotFound(id.to_string()))
        }

        fn archive_flow(&mut self, _caller: Caller, id: &FlowId) -> Result<FlowView> {
            Err(super::super::StorageError::NotFound(id.to_string()))
        }

        fn set_goal_status(
            &mut self,
            _caller: Caller,
            _id: &GoalId,
            _status: GoalStatus,
        ) -> Result<()> {
            Ok(())
        }

        fn terminate_session(&mut self, _caller: Caller, id: &SessionId) -> Result<Session> {
            Err(super::super::StorageError::NotFound(id.to_string()))
        }

        fn create_session(&mut self, _command: CreateSession<'_>) -> Result<()> {
            self.creations += 1;
            Ok(())
        }

        fn create_session_once(
            &mut self,
            _command: CreateSession<'_>,
            operation: &MutationOperation<'_>,
        ) -> Result<String> {
            Ok(operation.response_payload.to_owned())
        }

        fn create_execution(&mut self, _command: CreateExecution<'_>) -> Result<()> {
            self.creations += 1;
            Ok(())
        }
    }

    #[test]
    fn command_contract_can_be_mocked_without_sqlite() {
        let flow = Flow::new("Offline", "user");
        let edge = ProvenanceEdge {
            from: ObjectRef::from(&flow.id),
            relation: ProvenanceRelation::CreatedBy,
            to: ObjectRef::actor("user").expect("actor"),
            created_at: Timestamp::now(),
            note: None,
        };
        let mut mock = RecordingRepository { creations: 0 };
        create_through_trait(&mut mock, &flow, &edge);
        assert_eq!(mock.creations, 1);
    }
}
