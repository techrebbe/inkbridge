use inkbridge_broker::{
    blob, sha256_hex, state_path, Blob, BrokerStorage, CommitError, ConditionalWrite,
    GenerationPrecondition, StoredObject,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PayloadRef {
    pub path: String,
    pub generation: u64,
    pub content_sha256: String,
    pub size: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OutboxWrite {
    pub path: String,
    pub payload: PayloadRef,
    pub metadata: BTreeMap<String, String>,
    pub precondition: GenerationPrecondition,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ActiveState {
    pub payload: PayloadRef,
    pub generation: u64,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PendingCommit {
    pub commit_id: String,
    pub document_id: String,
    pub state_write: OutboxWrite,
    pub object_writes: Vec<OutboxWrite>,
    #[serde(default)]
    pub release_writes: Vec<OutboxWrite>,
    #[serde(default)]
    pub delivered: BTreeMap<String, PayloadRef>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StateRecord {
    pub active: Option<ActiveState>,
    pub pending: Option<PendingCommit>,
    #[serde(skip)]
    pub update_token: Option<String>,
}

pub trait CanonicalStateStore: Send + Sync {
    fn load(&self, document_id: &str) -> Result<StateRecord, String>;
    fn reserve(&self, pending: &PendingCommit) -> Result<PendingCommit, String>;
    fn save_pending(&self, pending: &PendingCommit) -> Result<(), String>;
    fn finalize(&self, pending: &PendingCommit) -> Result<ActiveState, String>;
    fn complete(&self, pending: &PendingCommit) -> Result<(), String>;
}

pub trait ObjectStore: Send + Sync {
    fn read(&self, path: &str) -> Result<Option<StoredObject>, String>;
    fn read_generation(&self, path: &str, generation: u64) -> Result<Option<StoredObject>, String> {
        self.read(path)
            .map(|object| object.filter(|candidate| candidate.generation == generation))
    }
    fn conditional_write(&self, write: &ConditionalWrite) -> Result<StoredObject, CommitError>;
    fn delete_generation(&self, path: &str, generation: u64) -> Result<(), String>;
}

#[derive(Clone, Default)]
pub struct MemoryCanonicalStateStore {
    records: Arc<Mutex<BTreeMap<String, StateRecord>>>,
}

impl MemoryCanonicalStateStore {
    pub fn record(&self, document_id: &str) -> Option<StateRecord> {
        self.records.lock().unwrap().get(document_id).cloned()
    }
}

impl CanonicalStateStore for MemoryCanonicalStateStore {
    fn load(&self, document_id: &str) -> Result<StateRecord, String> {
        Ok(self
            .records
            .lock()
            .map_err(|_| "state lock was poisoned".to_owned())?
            .get(document_id)
            .cloned()
            .unwrap_or_default())
    }

    fn reserve(&self, pending: &PendingCommit) -> Result<PendingCommit, String> {
        let mut records = self
            .records
            .lock()
            .map_err(|_| "state lock was poisoned".to_owned())?;
        let record = records.entry(pending.document_id.clone()).or_default();
        if let Some(existing) = &record.pending {
            return if existing.commit_id == pending.commit_id {
                Ok(existing.clone())
            } else {
                Err(format!(
                    "document {} already has pending commit {}",
                    pending.document_id, existing.commit_id
                ))
            };
        }
        check_state_precondition(record.active.as_ref(), pending.state_write.precondition)?;
        record.pending = Some(pending.clone());
        Ok(pending.clone())
    }

    fn save_pending(&self, pending: &PendingCommit) -> Result<(), String> {
        let mut records = self
            .records
            .lock()
            .map_err(|_| "state lock was poisoned".to_owned())?;
        let record = records
            .get_mut(&pending.document_id)
            .ok_or_else(|| "pending state record disappeared".to_owned())?;
        match &record.pending {
            Some(existing) if existing.commit_id == pending.commit_id => {
                record.pending = Some(pending.clone());
                Ok(())
            }
            _ => Err("pending commit changed before delivery checkpoint".to_owned()),
        }
    }

    fn finalize(&self, pending: &PendingCommit) -> Result<ActiveState, String> {
        let mut records = self
            .records
            .lock()
            .map_err(|_| "state lock was poisoned".to_owned())?;
        let record = records
            .get_mut(&pending.document_id)
            .ok_or_else(|| "pending state record disappeared".to_owned())?;
        let current = record
            .pending
            .as_ref()
            .ok_or_else(|| "pending commit was already cleared".to_owned())?;
        if current.commit_id != pending.commit_id {
            return Err("a different pending commit replaced this one".to_owned());
        }
        if !pending
            .object_writes
            .iter()
            .all(|write| pending.delivered.contains_key(&write.path))
        {
            return Err("cannot publish state before every object is delivered".to_owned());
        }
        if let Some(active) = record.active.as_ref() {
            if active.payload == pending.state_write.payload
                && active.metadata == pending.state_write.metadata
            {
                return Ok(active.clone());
            }
        }
        let generation = record
            .active
            .as_ref()
            .map_or(1, |state| state.generation + 1);
        let active = ActiveState {
            payload: pending.state_write.payload.clone(),
            generation,
            metadata: pending.state_write.metadata.clone(),
        };
        record.active = Some(active.clone());
        if pending.release_writes.is_empty() {
            record.pending = None;
        }
        Ok(active)
    }

    fn complete(&self, pending: &PendingCommit) -> Result<(), String> {
        let mut records = self
            .records
            .lock()
            .map_err(|_| "state lock was poisoned".to_owned())?;
        let record = records
            .get_mut(&pending.document_id)
            .ok_or_else(|| "pending state record disappeared".to_owned())?;
        let current = record
            .pending
            .as_ref()
            .ok_or_else(|| "pending commit disappeared before completion".to_owned())?;
        if current.commit_id != pending.commit_id {
            return Err("a different pending commit replaced this one".to_owned());
        }
        let active = record
            .active
            .as_ref()
            .ok_or_else(|| "canonical state was not finalized before completion".to_owned())?;
        if active.payload != pending.state_write.payload
            || active.metadata != pending.state_write.metadata
            || !pending
                .release_writes
                .iter()
                .all(|write| pending.delivered.contains_key(&write.path))
        {
            return Err(
                "cannot complete commit before every release signal is delivered".to_owned(),
            );
        }
        record.pending = None;
        Ok(())
    }
}

fn check_state_precondition(
    active: Option<&ActiveState>,
    precondition: GenerationPrecondition,
) -> Result<(), String> {
    let actual = active.map(|state| state.generation);
    let matches = match precondition {
        GenerationPrecondition::DoesNotExist => actual.is_none(),
        GenerationPrecondition::Match(expected) => actual == Some(expected),
    };
    if matches {
        Ok(())
    } else {
        Err(format!(
            "canonical state generation precondition failed: expected {:?}, actual {actual:?}",
            precondition
        ))
    }
}

#[derive(Clone, Default)]
pub struct MemoryObjectStore {
    inner: Arc<Mutex<MemoryObjectState>>,
}

#[derive(Default)]
struct MemoryObjectState {
    objects: BTreeMap<String, StoredObject>,
    versions: BTreeMap<(String, u64), StoredObject>,
    reads: BTreeMap<String, usize>,
    next_generation: u64,
    fail_after_writes: Option<usize>,
    successful_writes: usize,
}

impl MemoryObjectStore {
    pub fn put(&self, path: impl Into<String>, bytes: Vec<u8>) -> StoredObject {
        let mut inner = self.inner.lock().unwrap();
        inner.next_generation += 1;
        let path = path.into();
        let object = StoredObject {
            bytes: blob(bytes),
            generation: inner.next_generation,
            metadata: BTreeMap::new(),
        };
        inner.objects.insert(path.clone(), object.clone());
        inner
            .versions
            .insert((path, object.generation), object.clone());
        object
    }

    pub fn fail_after_writes(&self, count: usize) {
        let mut inner = self.inner.lock().unwrap();
        inner.fail_after_writes = Some(count);
        inner.successful_writes = 0;
    }

    pub fn clear_failure(&self) {
        self.inner.lock().unwrap().fail_after_writes = None;
    }

    pub fn read_count(&self, path: &str) -> usize {
        self.inner
            .lock()
            .unwrap()
            .reads
            .get(path)
            .copied()
            .unwrap_or_default()
    }
}

impl ObjectStore for MemoryObjectStore {
    fn read(&self, path: &str) -> Result<Option<StoredObject>, String> {
        let mut inner = self.inner.lock().unwrap();
        *inner.reads.entry(path.to_owned()).or_default() += 1;
        Ok(inner.objects.get(path).cloned())
    }

    fn read_generation(&self, path: &str, generation: u64) -> Result<Option<StoredObject>, String> {
        let mut inner = self.inner.lock().unwrap();
        *inner.reads.entry(path.to_owned()).or_default() += 1;
        Ok(inner.versions.get(&(path.to_owned(), generation)).cloned())
    }

    fn conditional_write(&self, write: &ConditionalWrite) -> Result<StoredObject, CommitError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| CommitError::Other("object lock was poisoned".to_owned()))?;
        if inner
            .fail_after_writes
            .is_some_and(|limit| inner.successful_writes >= limit)
        {
            return Err(CommitError::Other(
                "injected object delivery failure".to_owned(),
            ));
        }
        let actual = inner
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
        inner.next_generation += 1;
        inner.successful_writes += 1;
        let object = StoredObject {
            bytes: write.bytes.clone(),
            generation: inner.next_generation,
            metadata: write.metadata.clone(),
        };
        inner.objects.insert(write.path.clone(), object.clone());
        inner
            .versions
            .insert((write.path.clone(), object.generation), object.clone());
        Ok(object)
    }

    fn delete_generation(&self, path: &str, generation: u64) -> Result<(), String> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "object lock was poisoned".to_owned())?;
        if inner
            .objects
            .get(path)
            .is_some_and(|object| object.generation == generation)
        {
            inner.objects.remove(path);
        }
        inner.versions.remove(&(path.to_owned(), generation));
        Ok(())
    }
}

pub struct CloudBrokerStorage {
    objects: Arc<dyn ObjectStore>,
    states: Arc<dyn CanonicalStateStore>,
}

impl CloudBrokerStorage {
    pub fn new(objects: Arc<dyn ObjectStore>, states: Arc<dyn CanonicalStateStore>) -> Self {
        Self { objects, states }
    }

    pub fn recover(&self, document_id: &str) -> Result<bool, String> {
        let record = self.states.load(document_id)?;
        let Some(pending) = record.pending else {
            return Ok(false);
        };
        let canonical_already_active = record.active.as_ref().is_some_and(|active| {
            active.payload == pending.state_write.payload
                && active.metadata == pending.state_write.metadata
        });
        self.deliver(pending, canonical_already_active)
            .map(|_| true)
            .map_err(|error| format!("pending outbox recovery failed: {error:?}"))
    }

    fn deliver(
        &self,
        mut pending: PendingCommit,
        canonical_already_active: bool,
    ) -> Result<Vec<StoredObject>, CommitError> {
        let mut delivered_objects =
            Vec::with_capacity(pending.object_writes.len() + pending.release_writes.len());
        if !canonical_already_active {
            for write in pending.object_writes.clone() {
                delivered_objects.push(self.deliver_write(&mut pending, &write)?);
            }
        }

        self.states.finalize(&pending).map_err(CommitError::Other)?;

        // A conflict-resolution marker is an externally visible unblock signal. It may
        // become visible only after the canonical state is active. Keeping it in the
        // durable pending commit lets recovery finish this phase after a crash.
        for write in pending.release_writes.clone() {
            delivered_objects.push(self.deliver_write(&mut pending, &write)?);
        }
        if !pending.release_writes.is_empty() {
            self.states.complete(&pending).map_err(CommitError::Other)?;
        }

        // Only finalized commits may release their immutable delivery payloads. Cleanup is
        // generation-conditional and best-effort: a failure can leak storage, but it must never
        // roll back published state or make a pending commit unrecoverable.
        for write in pending.object_writes.iter().chain(&pending.release_writes) {
            if let Err(error) = self
                .objects
                .delete_generation(&write.payload.path, write.payload.generation)
            {
                eprintln!(
                    "failed to remove delivered outbox payload {}@{}: {error}",
                    write.payload.path, write.payload.generation
                );
            }
        }
        Ok(delivered_objects)
    }

    fn deliver_write(
        &self,
        pending: &mut PendingCommit,
        write: &OutboxWrite,
    ) -> Result<StoredObject, CommitError> {
        let bytes = self.read_payload(&write.payload)?;
        let object = if let Some(delivered) = pending.delivered.get(&write.path) {
            let current = self
                .objects
                .read(&write.path)
                .map_err(CommitError::Other)?
                .ok_or_else(|| {
                    CommitError::Other(format!("delivered object {} disappeared", write.path))
                })?;
            if current.generation != delivered.generation
                || current.bytes != bytes
                || current.metadata != write.metadata
            {
                return Err(CommitError::Other(format!(
                    "delivered object {} changed before outbox finalization",
                    write.path
                )));
            }
            current
        } else if let Some(current) = self.objects.read(&write.path).map_err(CommitError::Other)? {
            if current.bytes == bytes && current.metadata == write.metadata {
                current
            } else {
                self.objects
                    .conditional_write(&materialize_write(write, bytes.clone()))?
            }
        } else {
            self.objects
                .conditional_write(&materialize_write(write, bytes.clone()))?
        };
        pending.delivered.insert(
            write.path.clone(),
            PayloadRef {
                path: write.path.clone(),
                generation: object.generation,
                content_sha256: sha256_hex(&object.bytes),
                size: object.bytes.len() as u64,
            },
        );
        self.states
            .save_pending(pending)
            .map_err(CommitError::Other)?;
        Ok(object)
    }

    fn read_payload(&self, payload: &PayloadRef) -> Result<Blob, CommitError> {
        let object = self
            .objects
            .read_generation(&payload.path, payload.generation)
            .map_err(CommitError::Other)?
            .ok_or_else(|| {
                CommitError::Other(format!("outbox payload {} disappeared", payload.path))
            })?;
        let actual_hash = sha256_hex(&object.bytes);
        if object.generation != payload.generation
            || actual_hash != payload.content_sha256
            || object.bytes.len() as u64 != payload.size
        {
            return Err(CommitError::Other(format!(
                "outbox payload {} failed its immutable generation/hash check",
                payload.path
            )));
        }
        Ok(object.bytes)
    }

    fn stage_payload(
        &self,
        document_id: &str,
        commit_id: &str,
        index: usize,
        write: &ConditionalWrite,
        is_state: bool,
    ) -> Result<OutboxWrite, CommitError> {
        let payload_path = if is_state {
            format!("Canonical/{document_id}/states/{commit_id}.json")
        } else {
            format!("BrokerOutbox/{document_id}/{commit_id}/{index:04}.payload")
        };
        let hash = sha256_hex(&write.bytes);
        let payload_write = ConditionalWrite {
            path: payload_path.clone(),
            bytes: write.bytes.clone(),
            metadata: BTreeMap::from([
                ("inkbridge-kind".to_owned(), "outbox-payload".to_owned()),
                ("inkbridge-commit-id".to_owned(), commit_id.to_owned()),
                ("inkbridge-content-sha256".to_owned(), hash.clone()),
            ]),
            precondition: GenerationPrecondition::DoesNotExist,
        };
        let payload = match self
            .objects
            .read(&payload_path)
            .map_err(CommitError::Other)?
        {
            Some(existing)
                if existing.bytes == payload_write.bytes
                    && existing.metadata == payload_write.metadata =>
            {
                existing
            }
            Some(existing) => {
                return Err(CommitError::PreconditionFailed {
                    path: payload_path,
                    expected: GenerationPrecondition::DoesNotExist,
                    actual: Some(existing.generation),
                });
            }
            None => self.objects.conditional_write(&payload_write)?,
        };
        Ok(OutboxWrite {
            path: write.path.clone(),
            payload: PayloadRef {
                path: payload_write.path,
                generation: payload.generation,
                content_sha256: hash,
                size: payload.bytes.len() as u64,
            },
            metadata: write.metadata.clone(),
            precondition: write.precondition,
        })
    }
}

impl BrokerStorage for CloudBrokerStorage {
    fn read(&self, path: &str) -> Result<Option<StoredObject>, String> {
        if let Some(document_id) = document_id_from_state_path(path) {
            let active = self.states.load(document_id)?.active;
            return active
                .map(|active| {
                    self.read_payload(&active.payload)
                        .map(|bytes| StoredObject {
                            bytes,
                            generation: active.generation,
                            metadata: active.metadata,
                        })
                        .map_err(|error| format!("{error:?}"))
                })
                .transpose();
        }
        self.objects.read(path)
    }

    fn read_generation(&self, path: &str, generation: u64) -> Result<Option<StoredObject>, String> {
        if document_id_from_state_path(path).is_some() {
            return self
                .read(path)
                .map(|object| object.filter(|candidate| candidate.generation == generation));
        }
        self.objects.read_generation(path, generation)
    }

    fn commit(&mut self, writes: Vec<ConditionalWrite>) -> Result<Vec<StoredObject>, CommitError> {
        let state_index = writes
            .iter()
            .position(|write| document_id_from_state_path(&write.path).is_some())
            .ok_or_else(|| {
                CommitError::Other("broker commit has no canonical state write".to_owned())
            })?;
        if writes.iter().enumerate().any(|(index, write)| {
            index != state_index && document_id_from_state_path(&write.path).is_some()
        }) {
            return Err(CommitError::Other(
                "broker commit has more than one canonical state write".to_owned(),
            ));
        }
        let state_write = writes[state_index].clone();
        let document_id = document_id_from_state_path(&state_write.path)
            .expect("state write was already identified")
            .to_owned();
        let all_object_writes = writes
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != state_index)
            .map(|(_, write)| write.clone())
            .collect::<Vec<_>>();
        for write in &all_object_writes {
            let current = self.objects.read(&write.path).map_err(CommitError::Other)?;
            let already_delivered = current.as_ref().is_some_and(|object| {
                object.bytes == write.bytes && object.metadata == write.metadata
            });
            let matches = match write.precondition {
                GenerationPrecondition::DoesNotExist => current.is_none(),
                GenerationPrecondition::Match(expected) => {
                    current.as_ref().map(|object| object.generation) == Some(expected)
                }
            };
            if !matches && !already_delivered {
                return Err(CommitError::PreconditionFailed {
                    path: write.path.clone(),
                    expected: write.precondition,
                    actual: current.map(|object| object.generation),
                });
            }
        }
        let commit_id = commit_id(&document_id, &state_write, &all_object_writes);
        let staged_state =
            self.stage_payload(&document_id, &commit_id, state_index, &state_write, true)?;
        let mut staged_objects = Vec::new();
        let mut staged_releases = Vec::new();
        for (index, write) in all_object_writes.iter().enumerate() {
            let staged = self.stage_payload(&document_id, &commit_id, index, write, false)?;
            if is_post_finalize_release(write) {
                staged_releases.push(staged);
            } else {
                staged_objects.push(staged);
            }
        }
        let pending = self
            .states
            .reserve(&PendingCommit {
                commit_id,
                document_id,
                state_write: staged_state,
                object_writes: staged_objects,
                release_writes: staged_releases,
                delivered: BTreeMap::new(),
            })
            .map_err(CommitError::Other)?;
        let delivered = self.deliver(pending, false)?;
        let active = self
            .states
            .load(document_id_from_state_path(&writes[state_index].path).unwrap())
            .map_err(CommitError::Other)?
            .active
            .ok_or_else(|| CommitError::Other("outbox did not publish active state".to_owned()))?;
        let state = StoredObject {
            bytes: self.read_payload(&active.payload)?,
            generation: active.generation,
            metadata: active.metadata,
        };
        let by_path = delivered
            .iter()
            .zip(
                writes
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| *index != state_index)
                    .map(|(_, write)| &write.path),
            )
            .map(|(object, path)| (path.clone(), object.clone()))
            .collect::<BTreeMap<_, _>>();
        writes
            .iter()
            .enumerate()
            .map(|(index, write)| {
                if index == state_index {
                    Ok(state.clone())
                } else {
                    by_path.get(&write.path).cloned().ok_or_else(|| {
                        CommitError::Other(format!("missing delivered object {}", write.path))
                    })
                }
            })
            .collect()
    }
}

fn commit_id(
    document_id: &str,
    state_write: &ConditionalWrite,
    object_writes: &[ConditionalWrite],
) -> String {
    let mut digest = Sha256::new();
    hash_field(&mut digest, document_id.as_bytes());
    hash_write(&mut digest, state_write);
    digest.update((object_writes.len() as u64).to_be_bytes());
    for write in object_writes {
        hash_write(&mut digest, write);
    }
    let hash = digest.finalize();
    format!("commit-{hash:x}")
}

fn hash_write(digest: &mut Sha256, write: &ConditionalWrite) {
    hash_field(digest, write.path.as_bytes());
    match write.precondition {
        GenerationPrecondition::DoesNotExist => digest.update([0]),
        GenerationPrecondition::Match(generation) => {
            digest.update([1]);
            digest.update(generation.to_be_bytes());
        }
    }
    digest.update((write.metadata.len() as u64).to_be_bytes());
    for (key, value) in &write.metadata {
        hash_field(digest, key.as_bytes());
        hash_field(digest, value.as_bytes());
    }
    // Hash the binary payload directly. Serializing Vec<u8> as JSON expands
    // every byte into a decimal array element and can multiply peak memory for
    // a normal multi-megabyte PDF.
    hash_field(digest, &write.bytes);
}

fn hash_field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

fn is_post_finalize_release(write: &ConditionalWrite) -> bool {
    write.path.starts_with("Conflicts/")
        && write.path.ends_with("/resolution.json")
        && write
            .metadata
            .get("inkbridge-kind")
            .is_some_and(|kind| kind == "conflict-resolution")
}

fn document_id_from_state_path(path: &str) -> Option<&str> {
    path.strip_prefix("Canonical/")?
        .strip_suffix("/state.json")
        .filter(|document_id| !document_id.is_empty() && state_path(document_id) == path)
}

fn materialize_write(write: &OutboxWrite, bytes: Blob) -> ConditionalWrite {
    ConditionalWrite {
        path: write.path.clone(),
        bytes,
        metadata: write.metadata.clone(),
        precondition: write.precondition,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Clone, Default)]
    struct FailOnceStateStore {
        inner: MemoryCanonicalStateStore,
        fail_next_finalize: Arc<AtomicBool>,
        fail_next_complete: Arc<AtomicBool>,
    }

    impl FailOnceStateStore {
        fn fail_finalize_once() -> Self {
            Self {
                inner: MemoryCanonicalStateStore::default(),
                fail_next_finalize: Arc::new(AtomicBool::new(true)),
                fail_next_complete: Arc::new(AtomicBool::new(false)),
            }
        }

        fn fail_complete_once() -> Self {
            Self {
                inner: MemoryCanonicalStateStore::default(),
                fail_next_finalize: Arc::new(AtomicBool::new(false)),
                fail_next_complete: Arc::new(AtomicBool::new(true)),
            }
        }

        fn record(&self, document_id: &str) -> Option<StateRecord> {
            self.inner.record(document_id)
        }
    }

    impl CanonicalStateStore for FailOnceStateStore {
        fn load(&self, document_id: &str) -> Result<StateRecord, String> {
            self.inner.load(document_id)
        }

        fn reserve(&self, pending: &PendingCommit) -> Result<PendingCommit, String> {
            self.inner.reserve(pending)
        }

        fn save_pending(&self, pending: &PendingCommit) -> Result<(), String> {
            self.inner.save_pending(pending)
        }

        fn finalize(&self, pending: &PendingCommit) -> Result<ActiveState, String> {
            if self.fail_next_finalize.swap(false, Ordering::SeqCst) {
                Err("injected canonical finalization failure".to_owned())
            } else {
                self.inner.finalize(pending)
            }
        }

        fn complete(&self, pending: &PendingCommit) -> Result<(), String> {
            if self.fail_next_complete.swap(false, Ordering::SeqCst) {
                Err("injected release completion failure".to_owned())
            } else {
                self.inner.complete(pending)
            }
        }
    }

    fn writes() -> Vec<ConditionalWrite> {
        vec![
            ConditionalWrite {
                path: "BOOX_Folder/doc/view.pdf".to_owned(),
                bytes: blob(b"view".to_vec()),
                metadata: BTreeMap::from([("source".to_owned(), "broker".to_owned())]),
                precondition: GenerationPrecondition::DoesNotExist,
            },
            ConditionalWrite {
                path: "Canonical/doc/accepted/input.json".to_owned(),
                bytes: blob(b"accepted".to_vec()),
                metadata: BTreeMap::new(),
                precondition: GenerationPrecondition::DoesNotExist,
            },
            ConditionalWrite {
                path: state_path("doc"),
                bytes: blob(b"state".to_vec()),
                metadata: BTreeMap::new(),
                precondition: GenerationPrecondition::DoesNotExist,
            },
        ]
    }

    #[test]
    fn conflict_marker_is_published_only_after_state_finalization_and_recovers() {
        let objects = Arc::new(MemoryObjectStore::default());
        let states = Arc::new(FailOnceStateStore::fail_finalize_once());
        let marker_path = "Conflicts/doc/event/resolution.json";
        let mut commit_writes = writes();
        commit_writes.insert(
            commit_writes.len() - 1,
            ConditionalWrite {
                path: marker_path.to_owned(),
                bytes: blob(b"resolved".to_vec()),
                metadata: BTreeMap::from([(
                    "inkbridge-kind".to_owned(),
                    "conflict-resolution".to_owned(),
                )]),
                precondition: GenerationPrecondition::DoesNotExist,
            },
        );

        let mut storage = CloudBrokerStorage::new(objects.clone(), states.clone());
        let error = storage.commit(commit_writes).unwrap_err();
        assert!(matches!(
            error,
            CommitError::Other(message)
                if message.contains("injected canonical finalization failure")
        ));
        assert!(objects.read(marker_path).unwrap().is_none());
        let interrupted = states.record("doc").unwrap();
        assert!(interrupted.active.is_none());
        assert!(interrupted.pending.is_some());

        assert!(storage.recover("doc").unwrap());
        assert_eq!(
            objects.read(marker_path).unwrap().unwrap().bytes.as_ref(),
            b"resolved"
        );
        let recovered = states.record("doc").unwrap();
        assert!(recovered.active.is_some());
        assert!(recovered.pending.is_none());
    }

    #[test]
    fn finalized_resolution_recovers_after_device_edits_a_released_view() {
        let objects = Arc::new(MemoryObjectStore::default());
        let states = Arc::new(FailOnceStateStore::fail_complete_once());
        let marker_path = "Conflicts/doc/event/resolution.json";
        let mut commit_writes = writes();
        commit_writes.insert(
            commit_writes.len() - 1,
            ConditionalWrite {
                path: marker_path.to_owned(),
                bytes: blob(b"resolved".to_vec()),
                metadata: BTreeMap::from([(
                    "inkbridge-kind".to_owned(),
                    "conflict-resolution".to_owned(),
                )]),
                precondition: GenerationPrecondition::DoesNotExist,
            },
        );

        let mut storage = CloudBrokerStorage::new(objects.clone(), states.clone());
        let error = storage.commit(commit_writes).unwrap_err();
        assert!(matches!(
            error,
            CommitError::Other(message)
                if message.contains("injected release completion failure")
        ));
        assert!(objects.read(marker_path).unwrap().is_some());
        let interrupted = states.record("doc").unwrap();
        assert!(interrupted.active.is_some());
        assert!(interrupted.pending.is_some());

        objects.put("BOOX_Folder/doc/view.pdf", b"device-edit".to_vec());
        assert!(storage.recover("doc").unwrap());
        assert_eq!(
            objects
                .read("BOOX_Folder/doc/view.pdf")
                .unwrap()
                .unwrap()
                .bytes
                .as_ref(),
            b"device-edit"
        );
        assert!(states.record("doc").unwrap().pending.is_none());
    }

    #[test]
    fn pending_outbox_resumes_without_rewriting_delivered_objects() {
        let objects = Arc::new(MemoryObjectStore::default());
        let states = Arc::new(MemoryCanonicalStateStore::default());
        // Three immutable payloads stage before reservation, then the first
        // destination is delivered. Fail on the second destination so retry
        // must resume a genuinely partial outbox.
        objects.fail_after_writes(4);
        let mut storage = CloudBrokerStorage::new(objects.clone(), states.clone());
        let error = storage.commit(writes()).unwrap_err();
        assert!(matches!(error, CommitError::Other(_)));
        assert!(storage.read(&state_path("doc")).unwrap().is_none());
        assert!(objects
            .inner
            .lock()
            .unwrap()
            .objects
            .keys()
            .any(|path| path.starts_with("BrokerOutbox/")));
        let first_generation = objects
            .read("BOOX_Folder/doc/view.pdf")
            .unwrap()
            .unwrap()
            .generation;

        objects.clear_failure();
        assert!(storage.recover("doc").unwrap());
        assert_eq!(
            objects
                .read("BOOX_Folder/doc/view.pdf")
                .unwrap()
                .unwrap()
                .generation,
            first_generation
        );
        assert_eq!(
            storage
                .read(&state_path("doc"))
                .unwrap()
                .unwrap()
                .bytes
                .as_ref(),
            b"state"
        );
        assert!(states.record("doc").unwrap().pending.is_none());
        let inner = objects.inner.lock().unwrap();
        assert!(inner
            .objects
            .keys()
            .all(|path| !path.starts_with("BrokerOutbox/")));
        assert!(inner
            .versions
            .keys()
            .all(|(path, _)| !path.starts_with("BrokerOutbox/")));
    }

    #[test]
    fn stale_destination_never_publishes_state() {
        let objects = Arc::new(MemoryObjectStore::default());
        let states = Arc::new(MemoryCanonicalStateStore::default());
        objects.put("BOOX_Folder/doc/view.pdf", b"newer".to_vec());
        let mut storage = CloudBrokerStorage::new(objects, states.clone());
        let error = storage.commit(writes()).unwrap_err();
        assert!(matches!(error, CommitError::PreconditionFailed { .. }));
        let record = states.record("doc").unwrap_or_default();
        assert!(record.active.is_none());
        assert!(record.pending.is_none());
    }

    #[test]
    fn commit_id_hashes_binary_payloads_and_every_generation_guard() {
        let writes = writes();
        let state = writes.last().unwrap();
        let objects = &writes[..writes.len() - 1];
        let original = commit_id("doc", state, objects);
        assert_eq!(original, commit_id("doc", state, objects));

        let mut changed_bytes = objects.to_vec();
        let mut payload = changed_bytes[0].bytes.to_vec();
        payload.push(0);
        changed_bytes[0].bytes = blob(payload);
        assert_ne!(original, commit_id("doc", state, &changed_bytes));

        let mut changed_guard = objects.to_vec();
        changed_guard[0].precondition = GenerationPrecondition::Match(7);
        assert_ne!(original, commit_id("doc", state, &changed_guard));
    }
}
