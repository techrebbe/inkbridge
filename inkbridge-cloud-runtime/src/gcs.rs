use crate::{bearer_headers, HttpBody, HttpRequest, HttpTransport, ObjectStore, TokenProvider};
use inkbridge_broker::{blob, CommitError, ConditionalWrite, GenerationPrecondition, StoredObject};
use percent_encoding::{percent_encode, NON_ALPHANUMERIC};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct GoogleCloudStorage {
    bucket: String,
    api_base: String,
    upload_base: String,
    transport: Arc<dyn HttpTransport>,
    tokens: Arc<dyn TokenProvider>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ObjectMetadata {
    generation: String,
    #[serde(default)]
    metadata: BTreeMap<String, String>,
}

impl GoogleCloudStorage {
    pub fn new(
        bucket: impl Into<String>,
        transport: Arc<dyn HttpTransport>,
        tokens: Arc<dyn TokenProvider>,
    ) -> Self {
        Self {
            bucket: bucket.into(),
            api_base: "https://storage.googleapis.com/storage/v1".to_owned(),
            upload_base: "https://storage.googleapis.com/upload/storage/v1".to_owned(),
            transport,
            tokens,
        }
    }

    #[cfg(test)]
    pub fn with_endpoints(mut self, api_base: &str, upload_base: &str) -> Self {
        self.api_base = api_base.to_owned();
        self.upload_base = upload_base.to_owned();
        self
    }

    fn encoded(value: &str) -> String {
        percent_encode(value.as_bytes(), NON_ALPHANUMERIC).to_string()
    }

    fn metadata_url(&self, path: &str) -> String {
        format!(
            "{}/b/{}/o/{}",
            self.api_base,
            Self::encoded(&self.bucket),
            Self::encoded(path)
        )
    }

    fn read_at(
        &self,
        path: &str,
        requested_generation: Option<u64>,
    ) -> Result<Option<StoredObject>, String> {
        let metadata_url = match requested_generation {
            Some(generation) => format!("{}?generation={generation}", self.metadata_url(path)),
            None => self.metadata_url(path),
        };
        let metadata_response = self.transport.execute(HttpRequest {
            method: "GET".to_owned(),
            url: metadata_url,
            headers: self.authorized_headers()?,
            body: HttpBody::empty(),
        })?;
        if metadata_response.status == 404 {
            return Ok(None);
        }
        if metadata_response.status != 200 {
            return Err(format!(
                "Cloud Storage metadata read for {path} returned HTTP {}",
                metadata_response.status
            ));
        }
        let metadata = self.parse_metadata(&metadata_response.body)?;
        let generation = metadata
            .generation
            .parse::<u64>()
            .map_err(|error| error.to_string())?;
        if requested_generation.is_some_and(|requested| requested != generation) {
            return Ok(None);
        }
        let media_response = self.transport.execute(HttpRequest {
            method: "GET".to_owned(),
            url: format!(
                "{}?alt=media&generation={generation}&ifGenerationMatch={generation}",
                self.metadata_url(path)
            ),
            headers: self.authorized_headers()?,
            body: HttpBody::empty(),
        })?;
        if media_response.status == 404 {
            return Ok(None);
        }
        if media_response.status != 200 {
            return Err(format!(
                "Cloud Storage media read for {path}@{generation} returned HTTP {}",
                media_response.status
            ));
        }
        Ok(Some(StoredObject {
            bytes: blob(media_response.body),
            generation,
            metadata: metadata.metadata,
        }))
    }

    fn authorized_headers(&self) -> Result<BTreeMap<String, String>, String> {
        self.tokens
            .access_token()
            .map(|token| bearer_headers(&token))
    }

    fn parse_metadata(&self, body: &[u8]) -> Result<ObjectMetadata, String> {
        serde_json::from_slice(body).map_err(|error| error.to_string())
    }

    fn current_generation(&self, path: &str) -> Result<Option<u64>, String> {
        let response = self.transport.execute(HttpRequest {
            method: "GET".to_owned(),
            url: self.metadata_url(path),
            headers: self.authorized_headers()?,
            body: HttpBody::empty(),
        })?;
        if response.status == 404 {
            return Ok(None);
        }
        if response.status != 200 {
            return Err(format!(
                "Cloud Storage metadata read for {path} returned HTTP {}",
                response.status
            ));
        }
        self.parse_metadata(&response.body)?
            .generation
            .parse::<u64>()
            .map(Some)
            .map_err(|error| error.to_string())
    }
}

impl ObjectStore for GoogleCloudStorage {
    fn read(&self, path: &str) -> Result<Option<StoredObject>, String> {
        self.read_at(path, None)
    }

    fn read_generation(&self, path: &str, generation: u64) -> Result<Option<StoredObject>, String> {
        self.read_at(path, Some(generation))
    }

    fn conditional_write(&self, write: &ConditionalWrite) -> Result<StoredObject, CommitError> {
        let expected_generation = match write.precondition {
            GenerationPrecondition::DoesNotExist => 0,
            GenerationPrecondition::Match(generation) => generation,
        };
        let object_metadata = serde_json::to_vec(&json!({
            "name": write.path,
            "metadata": write.metadata,
        }))
        .map_err(|error| CommitError::Other(error.to_string()))?;
        let mut headers = self.authorized_headers().map_err(CommitError::Other)?;
        headers.insert(
            "Content-Type".to_owned(),
            "application/json; charset=UTF-8".to_owned(),
        );
        headers.insert(
            "X-Upload-Content-Type".to_owned(),
            "application/octet-stream".to_owned(),
        );
        headers.insert(
            "X-Upload-Content-Length".to_owned(),
            write.bytes.len().to_string(),
        );
        let session = self
            .transport
            .execute(HttpRequest {
                method: "POST".to_owned(),
                url: format!(
                    "{}/b/{}/o?uploadType=resumable&ifGenerationMatch={expected_generation}",
                    self.upload_base,
                    Self::encoded(&self.bucket)
                ),
                headers,
                body: HttpBody::bytes(object_metadata),
            })
            .map_err(CommitError::Other)?;
        if session.status == 412 {
            let actual = self
                .current_generation(&write.path)
                .map_err(CommitError::Other)?;
            return Err(CommitError::PreconditionFailed {
                path: write.path.clone(),
                expected: write.precondition,
                actual,
            });
        }
        if !matches!(session.status, 200 | 201) {
            return Err(CommitError::Other(format!(
                "Cloud Storage resumable upload session for {} returned HTTP {}",
                write.path, session.status
            )));
        }
        let upload_url = session
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("location"))
            .map(|(_, value)| value.clone())
            .ok_or_else(|| {
                CommitError::Other(format!(
                    "Cloud Storage resumable upload session for {} returned no Location header",
                    write.path
                ))
            })?;
        let response = self
            .transport
            .execute(HttpRequest {
                method: "PUT".to_owned(),
                url: upload_url,
                headers: BTreeMap::from([
                    (
                        "Content-Type".to_owned(),
                        "application/octet-stream".to_owned(),
                    ),
                    ("Content-Length".to_owned(), write.bytes.len().to_string()),
                ]),
                body: HttpBody::shared(write.bytes.clone()),
            })
            .map_err(CommitError::Other)?;
        if response.status == 412 {
            return Err(CommitError::PreconditionFailed {
                path: write.path.clone(),
                expected: write.precondition,
                actual: None,
            });
        }
        if !matches!(response.status, 200 | 201) {
            return Err(CommitError::Other(format!(
                "Cloud Storage write for {} returned HTTP {}",
                write.path, response.status
            )));
        }
        let metadata = self
            .parse_metadata(&response.body)
            .map_err(CommitError::Other)?;
        Ok(StoredObject {
            bytes: write.bytes.clone(),
            generation: metadata
                .generation
                .parse()
                .map_err(|error: std::num::ParseIntError| CommitError::Other(error.to_string()))?,
            metadata: metadata.metadata,
        })
    }

    fn delete_generation(&self, path: &str, generation: u64) -> Result<(), String> {
        let response = self.transport.execute(HttpRequest {
            method: "DELETE".to_owned(),
            url: format!(
                "{}?generation={generation}&ifGenerationMatch={generation}",
                self.metadata_url(path)
            ),
            headers: self.authorized_headers()?,
            body: HttpBody::empty(),
        })?;
        if matches!(response.status, 200 | 204 | 404) {
            return Ok(());
        }
        Err(format!(
            "Cloud Storage generation delete for {path}@{generation} returned HTTP {}",
            response.status
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HttpResponse, StaticTokenProvider};
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

    #[test]
    fn create_uses_zero_generation_precondition_and_custom_metadata() {
        let transport = Arc::new(RecordingTransport::default());
        transport.responses.lock().unwrap().extend([
            HttpResponse {
                status: 200,
                headers: BTreeMap::from([(
                    "Location".to_owned(),
                    "https://upload.example/session".to_owned(),
                )]),
                body: Vec::new(),
            },
            HttpResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: br#"{"generation":"41","metadata":{"kind":"generated"}}"#.to_vec(),
            },
        ]);
        let storage = GoogleCloudStorage::new(
            "private bucket",
            transport.clone(),
            Arc::new(StaticTokenProvider("token".to_owned())),
        );
        let object = storage
            .conditional_write(&ConditionalWrite {
                path: "BOOX_Folder/doc/view.pdf".to_owned(),
                bytes: blob(b"pdf".to_vec()),
                metadata: BTreeMap::from([("kind".to_owned(), "generated".to_owned())]),
                precondition: GenerationPrecondition::DoesNotExist,
            })
            .unwrap();
        assert_eq!(object.generation, 41);
        let requests = transport.requests.lock().unwrap();
        assert!(requests[0].url.contains("uploadType=resumable"));
        assert!(requests[0].url.contains("ifGenerationMatch=0"));
        assert!(requests[0].url.contains("private%20bucket"));
        assert!(
            String::from_utf8_lossy(requests[0].body.as_ref()).contains("BOOX_Folder/doc/view.pdf")
        );
        assert_eq!(requests[1].url, "https://upload.example/session");
        assert!(matches!(requests[1].body, HttpBody::Shared(_)));
    }

    #[test]
    fn exact_generation_read_never_falls_forward_to_latest() {
        let transport = Arc::new(RecordingTransport::default());
        transport.responses.lock().unwrap().extend([
            HttpResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: br#"{"generation":"17","metadata":{}}"#.to_vec(),
            },
            HttpResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: b"old immutable generation".to_vec(),
            },
        ]);
        let storage = GoogleCloudStorage::new(
            "private-bucket",
            transport.clone(),
            Arc::new(StaticTokenProvider("token".to_owned())),
        );
        let object = storage
            .read_generation("BOOX_Folder/doc/view.pdf", 17)
            .unwrap()
            .unwrap();
        assert_eq!(object.bytes.as_ref(), b"old immutable generation");
        let requests = transport.requests.lock().unwrap();
        assert!(requests[0].url.ends_with("?generation=17"));
        assert!(requests[1].url.contains("alt=media&generation=17"));
    }

    #[test]
    fn outbox_cleanup_deletes_only_the_finalized_generation() {
        let transport = Arc::new(RecordingTransport::default());
        transport.responses.lock().unwrap().push(HttpResponse {
            status: 204,
            headers: BTreeMap::new(),
            body: Vec::new(),
        });
        let storage = GoogleCloudStorage::new(
            "private-bucket",
            transport.clone(),
            Arc::new(StaticTokenProvider("token".to_owned())),
        );

        storage
            .delete_generation("BrokerOutbox/doc/commit/0000.payload", 23)
            .unwrap();

        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests[0].method, "DELETE");
        assert!(requests[0].url.ends_with(
            "BrokerOutbox%2Fdoc%2Fcommit%2F0000%2Epayload?generation=23&ifGenerationMatch=23"
        ));
    }
}
