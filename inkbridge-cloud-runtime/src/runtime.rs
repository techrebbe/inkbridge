use crate::{
    hydrate_content_hash, translate_storage_finalized_event, CanonicalStateStore,
    CloudBrokerStorage, EventTranslation, ObjectStore,
};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use inkbridge_broker::{Broker, CanonicalDocumentState, ProcessOutcome};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct RuntimeService {
    bucket: String,
    objects: Arc<dyn ObjectStore>,
    states: Arc<dyn CanonicalStateStore>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterDocumentRequest {
    pub original_object_path: String,
    pub original_file_name: String,
    #[serde(default)]
    pub source_generation: Option<u64>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RuntimeOutcome {
    Ignored { reason: String },
    Processed { outcome: ProcessOutcome },
}

impl RuntimeService {
    pub fn new(
        bucket: impl Into<String>,
        objects: Arc<dyn ObjectStore>,
        states: Arc<dyn CanonicalStateStore>,
    ) -> Self {
        Self {
            bucket: bucket.into(),
            objects,
            states,
        }
    }

    pub fn handle_storage_event(
        &self,
        headers: &BTreeMap<String, String>,
        body: &[u8],
    ) -> Result<RuntimeOutcome, String> {
        let mut event = match translate_storage_finalized_event(&self.bucket, headers, body)? {
            EventTranslation::Process(event) => event,
            EventTranslation::Ignore { reason } => {
                return Ok(RuntimeOutcome::Ignored { reason });
            }
        };
        let storage = CloudBrokerStorage::new(self.objects.clone(), self.states.clone());
        storage.recover(&event.document_id)?;
        let source = self
            .objects
            .read(&event.object_path)?
            .ok_or_else(|| format!("source object {} does not exist", event.object_path))?;
        if source.generation != event.source_generation {
            // Let the broker record an older finalized event as stale. For a
            // supposedly newer generation, do not fabricate content bytes.
            if source.generation < event.source_generation {
                return Err(format!(
                    "event generation {} is newer than object generation {}",
                    event.source_generation, source.generation
                ));
            }
        } else {
            hydrate_content_hash(&mut event, &source.bytes)?;
        }
        let mut storage = storage;
        let outcome = Broker::default()
            .process(&mut storage, &event)
            .map_err(|error| error.to_string())?;
        Ok(RuntimeOutcome::Processed { outcome })
    }

    pub fn register_document(
        &self,
        request: &RegisterDocumentRequest,
    ) -> Result<CanonicalDocumentState, String> {
        let original = self
            .objects
            .read(&request.original_object_path)?
            .ok_or_else(|| {
                format!(
                    "registration source {} does not exist",
                    request.original_object_path
                )
            })?;
        if request
            .source_generation
            .is_some_and(|generation| generation != original.generation)
        {
            return Err(format!(
                "registration source generation changed: expected {:?}, found {}",
                request.source_generation, original.generation
            ));
        }
        let mut storage = CloudBrokerStorage::new(self.objects.clone(), self.states.clone());
        Broker::default()
            .register_document(&mut storage, &request.original_file_name, &original.bytes)
            .map_err(|error| error.to_string())
    }

    pub fn router(self) -> Router {
        Router::new()
            .route("/", post(eventarc_handler))
            .route("/healthz", get(health_handler))
            .route("/v1/documents/register", post(register_handler))
            .with_state(Arc::new(self))
    }
}

async fn eventarc_handler(
    State(service): State<Arc<RuntimeService>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let headers = header_map(&headers);
    match tokio::task::spawn_blocking(move || service.handle_storage_event(&headers, &body)).await {
        Ok(Ok(outcome)) => (StatusCode::OK, Json(json!(outcome))).into_response(),
        Ok(Err(error)) => error_response(StatusCode::INTERNAL_SERVER_ERROR, error),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
    }
}

async fn register_handler(
    State(service): State<Arc<RuntimeService>>,
    Json(request): Json<RegisterDocumentRequest>,
) -> Response {
    match tokio::task::spawn_blocking(move || service.register_document(&request)).await {
        Ok(Ok(state)) => (
            StatusCode::OK,
            Json(json!({
                "documentId": state.document_id,
                "originalSha256": state.original_pdf_sha256,
                "stateRevision": state.state_revision
            })),
        )
            .into_response(),
        Ok(Err(error)) => error_response(StatusCode::BAD_REQUEST, error),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
    }
}

async fn health_handler() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"status": "ok"})))
}

fn error_response(status: StatusCode, message: String) -> Response {
    (status, Json(json!({"error": message}))).into_response()
}

fn header_map(headers: &HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_owned(), value.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MemoryCanonicalStateStore, MemoryObjectStore};
    use inkbridge_broker::{boox_view_path, sha256_hex, DeviceSide, RevisionPair};
    use inkbridge_convert::{geometry_fingerprint, NativeStyle, StrokeSnapshot};
    use lopdf::{dictionary, Document, Object};

    fn original_pdf() -> Vec<u8> {
        let mut document = Document::with_version("1.7");
        let pages_id = document.new_object_id();
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! {},
        });
        document.objects.insert(
            pages_id,
            dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => 1,
            }
            .into(),
        );
        let catalog_id =
            document.add_object(dictionary! {"Type" => "Catalog", "Pages" => pages_id});
        document.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }

    fn headers(id: &str) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("ce-id".to_owned(), id.to_owned()),
            (
                "ce-type".to_owned(),
                crate::STORAGE_FINALIZED_EVENT_TYPE.to_owned(),
            ),
        ])
    }

    #[test]
    fn supernote_event_runs_through_cloud_outbox_and_duplicate_is_idempotent() {
        let objects = Arc::new(MemoryObjectStore::default());
        let states = Arc::new(MemoryCanonicalStateStore::default());
        let original = objects.put("Staging/original.pdf", original_pdf());
        let service = RuntimeService::new("sync-bucket", objects.clone(), states);
        let state = service
            .register_document(&RegisterDocumentRequest {
                original_object_path: "Staging/original.pdf".to_owned(),
                original_file_name: "daily-reading.pdf".to_owned(),
                source_generation: Some(original.generation),
            })
            .unwrap();
        let style = NativeStyle::default();
        let samples = vec![[0.2, 0.3, 900.0], [0.3, 0.4, 1100.0]];
        let stroke = StrokeSnapshot {
            source_uuid: "sn-1".to_owned(),
            origin: "supernote-native".to_owned(),
            page_index: 0,
            geometry_fingerprint: geometry_fingerprint(&style, &samples),
            native_style: style,
            samples,
        };
        let export = serde_json::to_vec(&json!({
            "sourceFileName": "daily-reading.pdf",
            "pageIndex": 0,
            "strokes": [{
                "sourceUuid": stroke.source_uuid,
                "sourceKey": stroke.source_uuid,
                "layerNum": stroke.native_style.layer_num,
                "thickness": stroke.native_style.thickness,
                "penColor": stroke.native_style.pen_color,
                "penType": stroke.native_style.pen_type,
                "samples": stroke.samples
            }]
        }))
        .unwrap();
        let path = format!("Supernote_Folder/{}/export.json", state.document_id);
        let source = objects.put(&path, export.clone());
        let body = serde_json::to_vec(&json!({
            "bucket": "sync-bucket",
            "name": path,
            "generation": source.generation.to_string(),
            "metadata": {
                "inkbridge-document-id": state.document_id.clone(),
                "inkbridge-source-revision": "1",
                "inkbridge-based-on-boox": "0",
                "inkbridge-based-on-supernote": "0",
                "inkbridge-content-sha256": sha256_hex(&export)
            }
        }))
        .unwrap();
        let first = service
            .handle_storage_event(&headers("event-1"), &body)
            .unwrap();
        assert!(matches!(
            first,
            RuntimeOutcome::Processed {
                outcome: ProcessOutcome::Applied { .. }
            }
        ));
        let active = states_record(&service, &state.document_id);
        let pdf_path = boox_view_path(&active);
        let generated = objects.read(&pdf_path).unwrap().unwrap();
        let second = service
            .handle_storage_event(&headers("event-1"), &body)
            .unwrap();
        assert!(matches!(
            second,
            RuntimeOutcome::Processed {
                outcome: ProcessOutcome::Duplicate { .. }
            }
        ));
        let output_event = serde_json::to_vec(&json!({
            "bucket": "sync-bucket",
            "name": pdf_path,
            "generation": generated.generation.to_string(),
            "metadata": generated.metadata
        }))
        .unwrap();
        let loop_outcome = service
            .handle_storage_event(&headers("broker-output-event"), &output_event)
            .unwrap();
        assert!(matches!(
            loop_outcome,
            RuntimeOutcome::Processed {
                outcome: ProcessOutcome::IgnoredBrokerOutput { .. }
            }
        ));
        assert_eq!(
            active.revisions(),
            RevisionPair {
                boox: 0,
                supernote: 1
            }
        );
        assert_eq!(active.device(DeviceSide::Supernote).revision, 1);
    }

    fn states_record(service: &RuntimeService, document_id: &str) -> CanonicalDocumentState {
        let record = service.states.load(document_id).unwrap();
        let active = record.active.unwrap();
        let payload = service.objects.read(&active.payload.path).unwrap().unwrap();
        serde_json::from_slice(&payload.bytes).unwrap()
    }
}
