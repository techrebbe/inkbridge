use crate::{bearer_headers, HttpRequest, HttpTransport, ObjectStore, TokenProvider};
use inkbridge_broker::{CommitError, ConditionalWrite, GenerationPrecondition, StoredObject};
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

    fn authorized_headers(&self) -> Result<BTreeMap<String, String>, String> {
        self.tokens
            .access_token()
            .map(|token| bearer_headers(&token))
    }

    fn parse_metadata(&self, body: &[u8]) -> Result<ObjectMetadata, String> {
        serde_json::from_slice(body).map_err(|error| error.to_string())
    }
}

impl ObjectStore for GoogleCloudStorage {
    fn read(&self, path: &str) -> Result<Option<StoredObject>, String> {
        let metadata_response = self.transport.execute(HttpRequest {
            method: "GET".to_owned(),
            url: self.metadata_url(path),
            headers: self.authorized_headers()?,
            body: Vec::new(),
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
        let media_response = self.transport.execute(HttpRequest {
            method: "GET".to_owned(),
            url: format!(
                "{}?alt=media&generation={generation}&ifGenerationMatch={generation}",
                self.metadata_url(path)
            ),
            headers: self.authorized_headers()?,
            body: Vec::new(),
        })?;
        if media_response.status != 200 {
            return Err(format!(
                "Cloud Storage media read for {path}@{generation} returned HTTP {}",
                media_response.status
            ));
        }
        Ok(Some(StoredObject {
            bytes: media_response.body,
            generation,
            metadata: metadata.metadata,
        }))
    }

    fn conditional_write(&self, write: &ConditionalWrite) -> Result<StoredObject, CommitError> {
        let expected_generation = match write.precondition {
            GenerationPrecondition::DoesNotExist => 0,
            GenerationPrecondition::Match(generation) => generation,
        };
        let boundary = format!(
            "inkbridge-{}",
            inkbridge_broker::sha256_hex(write.path.as_bytes())
        );
        let object_metadata = serde_json::to_vec(&json!({
            "name": write.path,
            "metadata": write.metadata,
        }))
        .map_err(|error| CommitError::Other(error.to_string()))?;
        let mut body = Vec::new();
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(&object_metadata);
        body.extend_from_slice(
            format!("\r\n--{boundary}\r\nContent-Type: application/octet-stream\r\n\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(&write.bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let mut headers = self.authorized_headers().map_err(CommitError::Other)?;
        headers.insert(
            "Content-Type".to_owned(),
            format!("multipart/related; boundary={boundary}"),
        );
        let response = self
            .transport
            .execute(HttpRequest {
                method: "POST".to_owned(),
                url: format!(
                    "{}/b/{}/o?uploadType=multipart&ifGenerationMatch={expected_generation}",
                    self.upload_base,
                    Self::encoded(&self.bucket)
                ),
                headers,
                body,
            })
            .map_err(CommitError::Other)?;
        if response.status == 412 {
            let actual = self
                .read(&write.path)
                .map_err(CommitError::Other)?
                .map(|object| object.generation);
            return Err(CommitError::PreconditionFailed {
                path: write.path.clone(),
                expected: write.precondition,
                actual,
            });
        }
        if response.status != 200 {
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
        transport.responses.lock().unwrap().push(HttpResponse {
            status: 200,
            headers: BTreeMap::new(),
            body: br#"{"generation":"41","metadata":{"kind":"generated"}}"#.to_vec(),
        });
        let storage = GoogleCloudStorage::new(
            "private bucket",
            transport.clone(),
            Arc::new(StaticTokenProvider("token".to_owned())),
        );
        let object = storage
            .conditional_write(&ConditionalWrite {
                path: "BOOX_Folder/doc/view.pdf".to_owned(),
                bytes: b"pdf".to_vec(),
                metadata: BTreeMap::from([("kind".to_owned(), "generated".to_owned())]),
                precondition: GenerationPrecondition::DoesNotExist,
            })
            .unwrap();
        assert_eq!(object.generation, 41);
        let requests = transport.requests.lock().unwrap();
        assert!(requests[0].url.contains("ifGenerationMatch=0"));
        assert!(requests[0].url.contains("private%20bucket"));
        assert!(String::from_utf8_lossy(&requests[0].body).contains("BOOX_Folder/doc/view.pdf"));
    }
}
