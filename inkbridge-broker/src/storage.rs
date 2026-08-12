use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

pub type Blob = Arc<[u8]>;

pub fn blob(bytes: Vec<u8>) -> Blob {
    Arc::from(bytes)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoredObject {
    pub bytes: Blob,
    pub generation: u64,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GenerationPrecondition {
    DoesNotExist,
    Match(u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConditionalWrite {
    pub path: String,
    pub bytes: Blob,
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

    fn read_generation(&self, path: &str, generation: u64) -> Result<Option<StoredObject>, String> {
        self.read(path)
            .map(|object| object.filter(|candidate| candidate.generation == generation))
    }

    /// Atomically checks every generation precondition and applies every write.
    /// A future Cloud adapter can implement this contract with a Firestore
    /// transaction plus a durable output outbox.
    fn commit(&mut self, writes: Vec<ConditionalWrite>) -> Result<Vec<StoredObject>, CommitError>;
}

#[derive(Clone, Debug, Default)]
pub struct MemoryStorage {
    objects: BTreeMap<String, StoredObject>,
    versions: BTreeMap<(String, u64), StoredObject>,
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
        let path = path.into();
        let object = StoredObject {
            bytes: blob(bytes),
            generation: self.next_generation,
            metadata,
        };
        self.objects.insert(path.clone(), object.clone());
        self.versions
            .insert((path, object.generation), object.clone());
        object
    }

    pub fn object(&self, path: &str) -> Option<&StoredObject> {
        self.objects.get(path)
    }

    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.objects.keys().map(String::as_str)
    }
}

impl BrokerStorage for MemoryStorage {
    fn read(&self, path: &str) -> Result<Option<StoredObject>, String> {
        Ok(self.objects.get(path).cloned())
    }

    fn read_generation(&self, path: &str, generation: u64) -> Result<Option<StoredObject>, String> {
        Ok(self.versions.get(&(path.to_owned(), generation)).cloned())
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
            self.objects.insert(write.path.clone(), object.clone());
            self.versions
                .insert((write.path, object.generation), object.clone());
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
                    bytes: blob(b"stale overwrite".to_vec()),
                    metadata: BTreeMap::new(),
                    precondition: GenerationPrecondition::Match(first.generation),
                },
                ConditionalWrite {
                    path: "state".to_owned(),
                    bytes: blob(b"must not commit".to_vec()),
                    metadata: BTreeMap::new(),
                    precondition: GenerationPrecondition::DoesNotExist,
                },
            ])
            .unwrap_err();
        assert!(matches!(error, CommitError::PreconditionFailed { .. }));
        assert_eq!(
            storage.object("destination").unwrap().bytes.as_ref(),
            b"two"
        );
        assert!(storage.object("state").is_none());
    }

    #[test]
    fn blob_clones_share_the_large_payload_allocation() {
        let bytes = blob(vec![7; 8 * 1024 * 1024]);
        let clone = bytes.clone();
        assert!(Arc::ptr_eq(&bytes, &clone));
    }
}
