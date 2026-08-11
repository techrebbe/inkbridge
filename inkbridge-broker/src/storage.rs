use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StoredObject {
    pub bytes: Vec<u8>,
    pub generation: u64,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GenerationPrecondition {
    DoesNotExist,
    Match(u64),
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConditionalWrite {
    pub path: String,
    pub bytes: Vec<u8>,
    pub metadata: BTreeMap<String, String>,
    pub precondition: GenerationPrecondition,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitError {
    PreconditionFailed {
        path: String,
        expected: GenerationPrecondition,
        actual: Option<u64>,
    },
    Other(String),
}

pub trait BrokerStorage {
    fn read(&self, path: &str) -> Result<Option<StoredObject>, String>;

    /// Atomically checks every generation precondition and applies every write.
    /// A future Cloud adapter can implement this contract with a Firestore
    /// transaction plus a durable output outbox.
    fn commit(&mut self, writes: Vec<ConditionalWrite>) -> Result<Vec<StoredObject>, CommitError>;
}

#[derive(Clone, Debug, Default)]
pub struct MemoryStorage {
    objects: BTreeMap<String, StoredObject>,
    next_generation: u64,
}

impl MemoryStorage {
    pub fn put_unchecked(
        &mut self,
        path: impl Into<String>,
        bytes: Vec<u8>,
        metadata: BTreeMap<String, String>,
    ) -> StoredObject {
        self.next_generation += 1;
        let object = StoredObject {
            bytes,
            generation: self.next_generation,
            metadata,
        };
        self.objects.insert(path.into(), object.clone());
        object
    }

    pub fn object(&self, path: &str) -> Option<&StoredObject> {
        self.objects.get(path)
    }
}

impl BrokerStorage for MemoryStorage {
    fn read(&self, path: &str) -> Result<Option<StoredObject>, String> {
        Ok(self.objects.get(path).cloned())
    }

    fn commit(&mut self, writes: Vec<ConditionalWrite>) -> Result<Vec<StoredObject>, CommitError> {
        for write in &writes {
            let actual = self
                .objects
                .get(&write.path)
                .map(|object| object.generation);
            let matches = match write.precondition {
                GenerationPrecondition::DoesNotExist => actual.is_none(),
                GenerationPrecondition::Match(expected) => actual == Some(expected),
            };
            if !matches {
                return Err(CommitError::PreconditionFailed {
                    path: write.path.clone(),
                    expected: write.precondition,
                    actual,
                });
            }
        }

        let mut committed = Vec::with_capacity(writes.len());
        for write in writes {
            self.next_generation += 1;
            let object = StoredObject {
                bytes: write.bytes,
                generation: self.next_generation,
                metadata: write.metadata,
            };
            self.objects.insert(write.path, object.clone());
            committed.push(object);
        }
        Ok(committed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_is_atomic_when_any_generation_precondition_is_stale() {
        let mut storage = MemoryStorage::default();
        let first = storage.put_unchecked("destination", b"one".to_vec(), BTreeMap::new());
        storage.put_unchecked("destination", b"two".to_vec(), BTreeMap::new());
        let error = storage
            .commit(vec![
                ConditionalWrite {
                    path: "destination".to_owned(),
                    bytes: b"stale overwrite".to_vec(),
                    metadata: BTreeMap::new(),
                    precondition: GenerationPrecondition::Match(first.generation),
                },
                ConditionalWrite {
                    path: "state".to_owned(),
                    bytes: b"must not commit".to_vec(),
                    metadata: BTreeMap::new(),
                    precondition: GenerationPrecondition::DoesNotExist,
                },
            ])
            .unwrap_err();
        assert!(matches!(error, CommitError::PreconditionFailed { .. }));
        assert_eq!(storage.object("destination").unwrap().bytes, b"two");
        assert!(storage.object("state").is_none());
    }
}
