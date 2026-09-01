use crate::{
    bearer_headers, ActiveState, CanonicalStateStore, HttpBody, HttpRequest, HttpTransport,
    PendingCommit, StateRecord, TokenProvider,
};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use inkbridge_broker::GenerationPrecondition;
use percent_encoding::{percent_encode, NON_ALPHANUMERIC};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

const FIRESTORE_SAFE_RECORD_BYTES: usize = 900_000;

#[derive(Clone)]
pub struct FirestoreCanonicalStateStore {
    project_id: String,
    database_id: String,
    api_base: String,
    transport: Arc<dyn HttpTransport>,
    tokens: Arc<dyn TokenProvider>,
}

#[derive(Clone, Debug)]
struct RemoteDocument {
    update_time: String,
    payload: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum OutboxStatus {
    Pending,
    Delivered,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct OutboxDocument {
    status: OutboxStatus,
    pending: PendingCommit,
}

impl FirestoreCanonicalStateStore {
    pub fn new(
        project_id: impl Into<String>,
        database_id: impl Into<String>,
        transport: Arc<dyn HttpTransport>,
        tokens: Arc<dyn TokenProvider>,
    ) -> Self {
        Self {
            project_id: project_id.into(),
            database_id: database_id.into(),
            api_base: "https://firestore.googleapis.com/v1".to_owned(),
            transport,
            tokens,
        }
    }

    #[cfg(test)]
    pub fn with_api_base(mut self, api_base: &str) -> Self {
        self.api_base = api_base.to_owned();
        self
    }

    fn database_root(&self) -> String {
        format!(
            "{}/projects/{}/databases/{}/documents",
            self.api_base,
            encode(&self.project_id),
            encode(&self.database_id)
        )
    }

    fn document_name(&self, collection: &str, id: &str) -> String {
        format!(
            "projects/{}/databases/{}/documents/{}/{}",
            self.project_id, self.database_id, collection, id
        )
    }

    fn get_document(&self, collection: &str, id: &str) -> Result<Option<RemoteDocument>, String> {
        let response = self.transport.execute(HttpRequest {
            method: "GET".to_owned(),
            url: format!("{}/{}/{}", self.database_root(), collection, encode(id)),
            headers: self.authorized_headers()?,
            body: HttpBody::empty(),
        })?;
        if response.status == 404 {
            return Ok(None);
        }
        if response.status != 200 {
            return Err(format!(
                "Firestore read {collection}/{id} returned HTTP {}",
                response.status
            ));
        }
        let value: Value =
            serde_json::from_slice(&response.body).map_err(|error| error.to_string())?;
        let update_time = value
            .get("updateTime")
            .and_then(Value::as_str)
            .ok_or_else(|| "Firestore document has no updateTime".to_owned())?
            .to_owned();
        let encoded = value
            .pointer("/fields/record/bytesValue")
            .and_then(Value::as_str)
            .ok_or_else(|| "Firestore document has no record bytesValue".to_owned())?;
        let payload = BASE64.decode(encoded).map_err(|error| error.to_string())?;
        Ok(Some(RemoteDocument {
            update_time,
            payload,
        }))
    }

    fn authorized_headers(&self) -> Result<BTreeMap<String, String>, String> {
        let mut headers = bearer_headers(&self.tokens.access_token()?);
        headers.insert("Content-Type".to_owned(), "application/json".to_owned());
        Ok(headers)
    }

    fn update_write(
        &self,
        collection: &str,
        id: &str,
        payload: &[u8],
        update_time: Option<&str>,
    ) -> Value {
        let precondition = update_time.map_or_else(
            || json!({"exists": false}),
            |value| json!({"updateTime": value}),
        );
        json!({
            "update": {
                "name": self.document_name(collection, id),
                "fields": {
                    "record": {"bytesValue": BASE64.encode(payload)}
                }
            },
            "currentDocument": precondition
        })
    }

    fn commit(&self, writes: Vec<Value>) -> Result<(), String> {
        let body =
            serde_json::to_vec(&json!({"writes": writes})).map_err(|error| error.to_string())?;
        let response = self.transport.execute(HttpRequest {
            method: "POST".to_owned(),
            url: format!(
                "{}/projects/{}/databases/{}/documents:commit",
                self.api_base,
                encode(&self.project_id),
                encode(&self.database_id)
            ),
            headers: self.authorized_headers()?,
            body: HttpBody::bytes(body),
        })?;
        if response.status != 200 {
            let detail = String::from_utf8_lossy(&response.body);
            return Err(format!(
                "Firestore atomic commit returned HTTP {}: {}; retry the event because a concurrent reservation may have won",
                response.status,
                detail.trim()
            ));
        }
        Ok(())
    }

    fn state_remote(&self, document_id: &str) -> Result<(StateRecord, Option<String>), String> {
        let remote = self.get_document("inkbridgeDocuments", document_id)?;
        match remote {
            Some(remote) => {
                let mut record: StateRecord =
                    serde_json::from_slice(&remote.payload).map_err(|error| error.to_string())?;
                record.update_token = Some(remote.update_time.clone());
                Ok((record, Some(remote.update_time)))
            }
            None => Ok((StateRecord::default(), None)),
        }
    }

    fn encode_record<T: Serialize>(record: &T) -> Result<Vec<u8>, String> {
        let bytes = serde_json::to_vec(record).map_err(|error| error.to_string())?;
        if bytes.len() > FIRESTORE_SAFE_RECORD_BYTES {
            return Err(format!(
                "Firestore state/outbox record is {} bytes; the safe limit is {FIRESTORE_SAFE_RECORD_BYTES}. Split canonical state before processing this document",
                bytes.len()
            ));
        }
        Ok(bytes)
    }
}

impl CanonicalStateStore for FirestoreCanonicalStateStore {
    fn load(&self, document_id: &str) -> Result<StateRecord, String> {
        self.state_remote(document_id).map(|(record, _)| record)
    }

    fn reserve(&self, pending: &PendingCommit) -> Result<PendingCommit, String> {
        let (mut record, state_update_time) = self.state_remote(&pending.document_id)?;
        if let Some(existing) = record.pending.as_ref() {
            return if existing.commit_id == pending.commit_id {
                Ok(existing.clone())
            } else {
                Err(format!(
                    "document {} already has pending commit {}",
                    pending.document_id, existing.commit_id
                ))
            };
        }
        let actual = record.active.as_ref().map(|state| state.generation);
        let matches = match pending.state_write.precondition {
            GenerationPrecondition::DoesNotExist => actual.is_none(),
            GenerationPrecondition::Match(expected) => actual == Some(expected),
        };
        if !matches {
            return Err(format!(
                "canonical state generation precondition failed: expected {:?}, actual {actual:?}",
                pending.state_write.precondition
            ));
        }
        record.pending = Some(pending.clone());
        record.update_token = None;
        let outbox = OutboxDocument {
            status: OutboxStatus::Pending,
            pending: pending.clone(),
        };
        let state_bytes = Self::encode_record(&record)?;
        let outbox_bytes = Self::encode_record(&outbox)?;
        self.commit(vec![
            self.update_write(
                "inkbridgeDocuments",
                &pending.document_id,
                &state_bytes,
                state_update_time.as_deref(),
            ),
            self.update_write("inkbridgeOutbox", &pending.commit_id, &outbox_bytes, None),
        ])?;
        Ok(pending.clone())
    }

    fn save_pending(&self, pending: &PendingCommit) -> Result<(), String> {
        let (mut record, state_update_time) = self.state_remote(&pending.document_id)?;
        let current = record
            .pending
            .as_ref()
            .ok_or_else(|| "pending commit disappeared before checkpoint".to_owned())?;
        if current.commit_id != pending.commit_id {
            return Err("a different pending commit replaced this one".to_owned());
        }
        let outbox_remote = self
            .get_document("inkbridgeOutbox", &pending.commit_id)?
            .ok_or_else(|| "durable outbox document disappeared".to_owned())?;
        record.pending = Some(pending.clone());
        record.update_token = None;
        let outbox = OutboxDocument {
            status: OutboxStatus::Pending,
            pending: pending.clone(),
        };
        self.commit(vec![
            self.update_write(
                "inkbridgeDocuments",
                &pending.document_id,
                &Self::encode_record(&record)?,
                state_update_time.as_deref(),
            ),
            self.update_write(
                "inkbridgeOutbox",
                &pending.commit_id,
                &Self::encode_record(&outbox)?,
                Some(&outbox_remote.update_time),
            ),
        ])
    }

    fn finalize(&self, pending: &PendingCommit) -> Result<ActiveState, String> {
        if !pending
            .object_writes
            .iter()
            .all(|write| pending.delivered.contains_key(&write.path))
        {
            return Err("cannot publish state before every object is delivered".to_owned());
        }
        let (mut record, state_update_time) = self.state_remote(&pending.document_id)?;
        let current = record
            .pending
            .as_ref()
            .ok_or_else(|| "pending commit disappeared before finalization".to_owned())?;
        if current.commit_id != pending.commit_id {
            return Err("a different pending commit replaced this one".to_owned());
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
        record.update_token = None;

        if pending.release_writes.is_empty() {
            let outbox_remote = self
                .get_document("inkbridgeOutbox", &pending.commit_id)?
                .ok_or_else(|| "durable outbox document disappeared".to_owned())?;
            record.pending = None;
            let delivered = OutboxDocument {
                status: OutboxStatus::Delivered,
                pending: pending.clone(),
            };
            self.commit(vec![
                self.update_write(
                    "inkbridgeDocuments",
                    &pending.document_id,
                    &Self::encode_record(&record)?,
                    state_update_time.as_deref(),
                ),
                self.update_write(
                    "inkbridgeOutbox",
                    &pending.commit_id,
                    &Self::encode_record(&delivered)?,
                    Some(&outbox_remote.update_time),
                ),
            ])?;
        } else {
            record.pending = Some(pending.clone());
            self.commit(vec![self.update_write(
                "inkbridgeDocuments",
                &pending.document_id,
                &Self::encode_record(&record)?,
                state_update_time.as_deref(),
            )])?;
        }
        Ok(active)
    }

    fn complete(&self, pending: &PendingCommit) -> Result<(), String> {
        let (mut record, state_update_time) = self.state_remote(&pending.document_id)?;
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
        let outbox_remote = self
            .get_document("inkbridgeOutbox", &pending.commit_id)?
            .ok_or_else(|| "durable outbox document disappeared".to_owned())?;
        record.pending = None;
        record.update_token = None;
        let delivered = OutboxDocument {
            status: OutboxStatus::Delivered,
            pending: pending.clone(),
        };
        self.commit(vec![
            self.update_write(
                "inkbridgeDocuments",
                &pending.document_id,
                &Self::encode_record(&record)?,
                state_update_time.as_deref(),
            ),
            self.update_write(
                "inkbridgeOutbox",
                &pending.commit_id,
                &Self::encode_record(&delivered)?,
                Some(&outbox_remote.update_time),
            ),
        ])
    }
}

fn encode(value: &str) -> String {
    percent_encode(value.as_bytes(), NON_ALPHANUMERIC).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HttpResponse, StaticTokenProvider};
    use crate::{OutboxWrite, PayloadRef};
    use inkbridge_broker::GenerationPrecondition;
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingTransport {
        requests: Mutex<Vec<HttpRequest>>,
        responses: Mutex<Vec<HttpResponse>>,
    }

    impl HttpTransport for RecordingTransport {
        fn execute(&self, request: HttpRequest) -> Result<HttpResponse, String> {
            self.requests.lock().unwrap().push(request);
            Ok(self.responses.lock().unwrap().remove(0))
        }
    }

    fn response(status: u16, body: &[u8]) -> HttpResponse {
        HttpResponse {
            status,
            headers: BTreeMap::new(),
            body: body.to_vec(),
        }
    }

    #[test]
    fn first_reservation_atomically_creates_state_and_outbox_with_preconditions() {
        let transport = Arc::new(RecordingTransport::default());
        transport
            .responses
            .lock()
            .unwrap()
            .extend([response(404, b""), response(200, br#"{}"#)]);
        let store = FirestoreCanonicalStateStore::new(
            "project",
            "(default)",
            transport.clone(),
            Arc::new(StaticTokenProvider("token".to_owned())),
        );
        let pending = PendingCommit {
            commit_id: "commit-1".to_owned(),
            document_id: "doc-1".to_owned(),
            state_write: OutboxWrite {
                path: "Canonical/doc-1/state.json".to_owned(),
                payload: PayloadRef {
                    path: "Canonical/doc-1/states/commit-1.json".to_owned(),
                    generation: 1,
                    content_sha256: "hash".to_owned(),
                    size: 5,
                },
                metadata: BTreeMap::new(),
                precondition: GenerationPrecondition::DoesNotExist,
            },
            object_writes: Vec::new(),
            release_writes: Vec::new(),
            delivered: BTreeMap::new(),
        };
        store.reserve(&pending).unwrap();
        let requests = transport.requests.lock().unwrap();
        let commit: Value = serde_json::from_slice(requests[1].body.as_ref()).unwrap();
        let writes = commit["writes"].as_array().unwrap();
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0]["currentDocument"]["exists"], false);
        assert_eq!(writes[1]["currentDocument"]["exists"], false);
        assert!(requests[1]
            .url
            .ends_with("databases/%28default%29/documents:commit"));
    }
}
