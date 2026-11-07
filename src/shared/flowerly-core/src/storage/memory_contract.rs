//! Inactive, backend-independent optional memory capabilities (#83).
//!
//! This is a contract, not a storage implementation. No production code
//! constructs or activates a memory backend; core SQLite stays authoritative.

/// Owner is derived from authenticated admission, never the request body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MemoryOwner(pub u32);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryEntry {
    pub id: String,
    pub owner: MemoryOwner,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Embedding {
    pub memory_id: String,
    pub owner: MemoryOwner,
    pub model: String,
    pub version: u32,
    pub values: Vec<f32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoryError {
    Unauthorized,
    Invalid,
    NotFound,
    Unavailable,
    Conflict,
    Corrupt,
}

/// No SQL string, connection handle, arbitrary filesystem path or raw actor UID
/// may be supplied by the memory consumer.
pub trait MemoryRepository {
    fn insert(&mut self, actor: MemoryOwner, entry: MemoryEntry) -> Result<(), MemoryError>;
    fn get(&self, actor: MemoryOwner, id: &str) -> Result<Option<MemoryEntry>, MemoryError>;
    fn delete(&mut self, actor: MemoryOwner, id: &str) -> Result<bool, MemoryError>;
}

pub trait EmbeddingIndex {
    fn put(&mut self, actor: MemoryOwner, embedding: Embedding) -> Result<(), MemoryError>;
    fn nearest(
        &self, actor: MemoryOwner, model: &str, version: u32, query: &[f32], limit: usize,
    ) -> Result<Vec<String>, MemoryError>;
}

/// A controller is never given system-state APIs. Peer enrollment and
/// authentication must be independently proven before an implementation.
pub trait MemoryReplicationController {
    fn status(&self) -> Result<ReplicationStatus, MemoryError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplicationStatus {
    Disabled,
    Unavailable,
    Active,
}

pub fn validate_embedding(entry: &Embedding, dimension: usize) -> Result<(), MemoryError> {
    if dimension == 0 || dimension > 4096 || entry.model.is_empty()
        || entry.version == 0 || entry.values.len() != dimension
        || !entry.values.iter().all(|value| value.is_finite())
    {
        return Err(MemoryError::Invalid);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vectors_require_model_version_dimension_and_finite_values() {
        let mut vector = Embedding {
            memory_id: "one".into(), owner: MemoryOwner(1000),
            model: "offline".into(), version: 1, values: vec![0.5, -0.1],
        };
        assert_eq!(validate_embedding(&vector, 2), Ok(()));
        assert_eq!(validate_embedding(&vector, 3), Err(MemoryError::Invalid));
        vector.values[0] = f32::NAN;
        assert_eq!(validate_embedding(&vector, 2), Err(MemoryError::Invalid));
        vector.values[0] = f32::INFINITY;
        assert_eq!(validate_embedding(&vector, 2), Err(MemoryError::Invalid));
    }
    struct DenyByDefault;
    impl MemoryRepository for DenyByDefault {
        fn insert(&mut self, _: MemoryOwner, _: MemoryEntry) -> Result<(), MemoryError> {
            Err(MemoryError::Unavailable)
        }
        fn get(&self, _: MemoryOwner, _: &str) -> Result<Option<MemoryEntry>, MemoryError> {
            Err(MemoryError::Unavailable)
        }
        fn delete(&mut self, _: MemoryOwner, _: &str) -> Result<bool, MemoryError> {
            Err(MemoryError::Unavailable)
        }
    }
    #[test]
    fn disabled_memory_cannot_fabricate_success() {
        let mut backend = DenyByDefault;
        assert_eq!(backend.get(MemoryOwner(1000), "id"), Err(MemoryError::Unavailable));
        assert_eq!(backend.delete(MemoryOwner(1000), "id"), Err(MemoryError::Unavailable));
    }
}
