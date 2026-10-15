//! Typed, backend-agnostic core command/query seam (issue #83).
//!
//! Domain callers receive no SQL connection and cannot commit provenance
//! independently of the corresponding Flow/Goal creation. This is an
//! incremental adapter over the accepted single-file SQLite Store: it neither
//! creates a second backend nor changes existing service behavior.

use agentic_types::{Flow, FlowId, Goal, GoalId, ObjectRef, ProvenanceEdge};

use crate::{MutationOperation, Result, Store};

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

/// Minimal core command capability. Extend command-by-command after each
/// transaction and authorization invariant has been reviewed.
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
    fn activate_flow(&mut self, id: &FlowId) -> Result<()>;
}

/// Read-only, typed queries. No raw SQL or underlying driver handles escape.
pub trait CoreQueryRepository {
    fn flow(&self, id: &FlowId) -> Result<Option<Flow>>;
    fn goal(&self, id: &GoalId) -> Result<Option<Goal>>;
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

    fn activate_flow(&mut self, id: &FlowId) -> Result<()> {
        Store::activate_flow(self, id)
    }
}

impl CoreQueryRepository for Store {
    fn flow(&self, id: &FlowId) -> Result<Option<Flow>> {
        self.get_flow(id)
    }

    fn goal(&self, id: &GoalId) -> Result<Option<Goal>> {
        self.get_goal(id)
    }

    fn provenance_from(&self, from: &ObjectRef) -> Result<Vec<ProvenanceEdge>> {
        Store::provenance_from(self, from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentic_types::{ProvenanceRelation, Timestamp};

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
        assert_eq!(reader.flow(&flow.id).expect("query"), Some(flow.clone()));
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

        fn activate_flow(&mut self, _id: &FlowId) -> Result<()> {
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
