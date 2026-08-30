use crate::{
    CheckpointStore, DriveApi, DriveChangePage, OnboardingApproval, OnboardingApprovalStore,
    VersionedCheckpoint,
};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use inkbridge_cloud_runtime::{
    bearer_headers, HttpBody, HttpRequest, HttpTransport, TokenProvider,
};
use inkbridge_drive_gateway::{
    DriveChange, DriveFileRevision, DriveGatewayCheckpoint, PreparedDriveOutput,
};
use percent_encoding::{percent_encode, NON_ALPHANUMERIC};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn encode(value: &str) -> String {
    percent_encode(value.as_bytes(), NON_ALPHANUMERIC).to_string()
}

#[derive(Clone)]
pub struct GoogleSecretManager {
    project_id: String,
    api_base: String,
    transport: Arc<dyn HttpTransport>,
    tokens: Arc<dyn TokenProvider>,
}

#[derive(Deserialize)]
struct SecretPayload {
    payload: SecretPayloadData,
}

#[derive(Deserialize)]
struct SecretPayloadData {
    data: String,
}

impl GoogleSecretManager {
    pub fn new(
        project_id: impl Into<String>,
        transport: Arc<dyn HttpTransport>,
        tokens: Arc<dyn TokenProvider>,
    ) -> Self {
        Self {
            project_id: project_id.into(),
            api_base: "https://secretmanager.googleapis.com/v1".to_owned(),
            transport,
            tokens,
        }
    }

    #[cfg(test)]
    pub fn with_api_base(mut self, api_base: &str) -> Self {
        self.api_base = api_base.to_owned();
        self
    }

    pub fn access_latest(&self, secret_id: &str) -> Result<Vec<u8>, String> {
        if secret_id.trim().is_empty()
            || !secret_id.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            })
        {
            return Err("Secret Manager secret ID is invalid".to_owned());
        }
        let response = self.transport.execute(HttpRequest {
            method: "GET".to_owned(),
            url: format!(
                "{}/projects/{}/secrets/{}/versions/latest:access",
                self.api_base,
                encode(&self.project_id),
                encode(secret_id)
            ),
            headers: bearer_headers(&self.tokens.access_token()?),
            body: HttpBody::empty(),
        })?;
        if response.status != 200 {
            return Err(format!(
                "Secret Manager access for {secret_id} returned HTTP {}",
                response.status
            ));
        }
        let payload: SecretPayload =
            serde_json::from_slice(&response.body).map_err(|error| error.to_string())?;
        BASE64
            .decode(payload.payload.data)
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone)]
pub struct DriveOAuthTokenProvider {
    transport: Arc<dyn HttpTransport>,
    client_secret: Vec<u8>,
    refresh_token: Vec<u8>,
    cached: Arc<Mutex<Option<CachedDriveToken>>>,
}

#[derive(Clone)]
struct CachedDriveToken {
    value: String,
    refresh_at: Instant,
}

#[derive(Deserialize)]
struct OAuthClientFile {
    #[serde(default)]
    installed: Option<OAuthClient>,
    #[serde(default)]
    web: Option<OAuthClient>,
}

#[derive(Clone, Deserialize)]
struct OAuthClient {
    client_id: String,
    client_secret: String,
    #[serde(default = "default_token_uri")]
    token_uri: String,
}

fn default_token_uri() -> String {
    "https://oauth2.googleapis.com/token".to_owned()
}

#[derive(Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    expires_in: u64,
}

impl DriveOAuthTokenProvider {
    pub fn from_secret_manager(
        manager: &GoogleSecretManager,
        client_secret_id: &str,
        refresh_token_secret_id: &str,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, String> {
        Ok(Self {
            transport,
            client_secret: manager.access_latest(client_secret_id)?,
            refresh_token: manager.access_latest(refresh_token_secret_id)?,
            cached: Arc::new(Mutex::new(None)),
        })
    }

    #[cfg(test)]
    pub fn from_bytes(
        client_secret: Vec<u8>,
        refresh_token: Vec<u8>,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        Self {
            transport,
            client_secret,
            refresh_token,
            cached: Arc::new(Mutex::new(None)),
        }
    }

    fn client(&self) -> Result<OAuthClient, String> {
        let file: OAuthClientFile =
            serde_json::from_slice(&self.client_secret).map_err(|error| error.to_string())?;
        file.installed.or(file.web).ok_or_else(|| {
            "OAuth client secret contains neither installed nor web client".to_owned()
        })
    }
}

impl TokenProvider for DriveOAuthTokenProvider {
    fn access_token(&self) -> Result<String, String> {
        if let Some(cached) = self
            .cached
            .lock()
            .map_err(|_| "Drive token cache lock was poisoned".to_owned())?
            .as_ref()
            .filter(|token| token.refresh_at > Instant::now())
        {
            return Ok(cached.value.clone());
        }
        let client = self.client()?;
        let refresh_token = std::str::from_utf8(&self.refresh_token)
            .map_err(|error| error.to_string())?
            .trim();
        if refresh_token.is_empty() {
            return Err("Drive refresh token secret is empty".to_owned());
        }
        let body = format!(
            "client_id={}&client_secret={}&refresh_token={}&grant_type=refresh_token",
            encode(&client.client_id),
            encode(&client.client_secret),
            encode(refresh_token)
        );
        let response = self.transport.execute(HttpRequest {
            method: "POST".to_owned(),
            url: client.token_uri,
            headers: BTreeMap::from([(
                "Content-Type".to_owned(),
                "application/x-www-form-urlencoded".to_owned(),
            )]),
            body: HttpBody::bytes(body.into_bytes()),
        })?;
        if response.status != 200 {
            return Err(format!(
                "Google OAuth refresh returned HTTP {}",
                response.status
            ));
        }
        let token: OAuthTokenResponse =
            serde_json::from_slice(&response.body).map_err(|error| error.to_string())?;
        let cached = CachedDriveToken {
            value: token.access_token.clone(),
            refresh_at: Instant::now() + Duration::from_secs(token.expires_in.saturating_sub(60)),
        };
        *self
            .cached
            .lock()
            .map_err(|_| "Drive token cache lock was poisoned".to_owned())? = Some(cached);
        Ok(token.access_token)
    }
}

#[derive(Clone)]
pub struct GoogleDriveApi {
    api_base: String,
    upload_base: String,
    transport: Arc<dyn HttpTransport>,
    tokens: Arc<dyn TokenProvider>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriveFileWire {
    id: String,
    #[serde(default)]
    name: String,
    version: String,
    #[serde(default)]
    mime_type: String,
    #[serde(default)]
    parents: Vec<String>,
    #[serde(default)]
    size: String,
    #[serde(default)]
    trashed: bool,
    #[serde(default)]
    app_properties: BTreeMap<String, String>,
}

impl DriveFileWire {
    fn into_revision(self) -> Result<DriveFileRevision, String> {
        Ok(DriveFileRevision {
            file_id: self.id,
            name: self.name,
            version: self
                .version
                .parse()
                .map_err(|error| format!("invalid Drive file version: {error}"))?,
            mime_type: self.mime_type,
            parents: self.parents,
            size: if self.size.is_empty() {
                0
            } else {
                self.size
                    .parse()
                    .map_err(|error| format!("invalid Drive file size: {error}"))?
            },
            trashed: self.trashed,
            app_properties: self.app_properties,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriveChangesWire {
    #[serde(default)]
    changes: Vec<DriveChangeWire>,
    #[serde(default)]
    next_page_token: Option<String>,
    #[serde(default)]
    new_start_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriveChangeWire {
    file_id: String,
    #[serde(default)]
    removed: bool,
    #[serde(default)]
    file: Option<DriveFileWire>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriveFilesWire {
    #[serde(default)]
    files: Vec<DriveFileWire>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartPageToken {
    start_page_token: String,
}

impl GoogleDriveApi {
    pub fn new(transport: Arc<dyn HttpTransport>, tokens: Arc<dyn TokenProvider>) -> Self {
        Self {
            api_base: "https://www.googleapis.com/drive/v3".to_owned(),
            upload_base: "https://www.googleapis.com/upload/drive/v3".to_owned(),
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

    fn headers(&self) -> Result<BTreeMap<String, String>, String> {
        self.tokens
            .access_token()
            .map(|token| bearer_headers(&token))
    }

    fn file_fields() -> &'static str {
        "id,name,version,mimeType,parents,size,trashed,appProperties"
    }
}

impl DriveApi for GoogleDriveApi {
    fn start_page_token(&self) -> Result<String, String> {
        let response = self.transport.execute(HttpRequest {
            method: "GET".to_owned(),
            url: format!("{}/changes/startPageToken?spaces=drive", self.api_base),
            headers: self.headers()?,
            body: HttpBody::empty(),
        })?;
        if response.status != 200 {
            return Err(format!(
                "Drive start page token returned HTTP {}",
                response.status
            ));
        }
        let token: StartPageToken =
            serde_json::from_slice(&response.body).map_err(|error| error.to_string())?;
        Ok(token.start_page_token)
    }

    fn list_initial_files(&self, folder_ids: &[String]) -> Result<Vec<DriveFileRevision>, String> {
        let mut by_id = BTreeMap::new();
        for folder_id in folder_ids {
            if !folder_id.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            }) {
                return Err("Drive folder ID is invalid".to_owned());
            }
            let query = format!("'{folder_id}' in parents and trashed=false");
            let fields = format!("nextPageToken,files({})", Self::file_fields());
            let mut page_token = None;
            loop {
                let page = page_token
                    .as_ref()
                    .map(|token: &String| format!("&pageToken={}", encode(token)))
                    .unwrap_or_default();
                let response = self.transport.execute(HttpRequest {
                    method: "GET".to_owned(),
                    url: format!(
                        "{}/files?q={}&spaces=drive&pageSize=1000&fields={}{}",
                        self.api_base,
                        encode(&query),
                        encode(&fields),
                        page
                    ),
                    headers: self.headers()?,
                    body: HttpBody::empty(),
                })?;
                if response.status != 200 {
                    return Err(format!(
                        "Drive initial files.list returned HTTP {}",
                        response.status
                    ));
                }
                let wire: DriveFilesWire =
                    serde_json::from_slice(&response.body).map_err(|error| error.to_string())?;
                for file in wire.files {
                    let revision = file.into_revision()?;
                    by_id.insert(revision.file_id.clone(), revision);
                }
                page_token = wire.next_page_token;
                if page_token.is_none() {
                    break;
                }
            }
        }
        Ok(by_id.into_values().collect())
    }

    fn list_changes(&self, page_token: &str) -> Result<DriveChangePage, String> {
        let fields = format!(
            "nextPageToken,newStartPageToken,changes(fileId,removed,file({}))",
            Self::file_fields()
        );
        let response = self.transport.execute(HttpRequest {
            method: "GET".to_owned(),
            url: format!(
                "{}/changes?pageToken={}&spaces=drive&includeRemoved=true&restrictToMyDrive=true&fields={}",
                self.api_base,
                encode(page_token),
                encode(&fields)
            ),
            headers: self.headers()?,
            body: HttpBody::empty(),
        })?;
        if response.status != 200 {
            return Err(format!(
                "Drive changes.list returned HTTP {}",
                response.status
            ));
        }
        let wire: DriveChangesWire =
            serde_json::from_slice(&response.body).map_err(|error| error.to_string())?;
        let changes = wire
            .changes
            .into_iter()
            .map(|change| {
                let file = match change.file {
                    Some(file) => file.into_revision()?,
                    None if change.removed => DriveFileRevision {
                        file_id: change.file_id,
                        name: String::new(),
                        version: 1,
                        mime_type: String::new(),
                        parents: Vec::new(),
                        size: 0,
                        trashed: true,
                        app_properties: BTreeMap::new(),
                    },
                    None => return Err("Drive change has no file resource".to_owned()),
                };
                Ok(DriveChange { file })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(DriveChangePage {
            changes,
            next_page_token: wire.next_page_token,
            new_start_page_token: wire.new_start_page_token,
        })
    }

    fn download(&self, file_id: &str) -> Result<Vec<u8>, String> {
        let response = self.transport.execute(HttpRequest {
            method: "GET".to_owned(),
            url: format!("{}/files/{}?alt=media", self.api_base, encode(file_id)),
            headers: self.headers()?,
            body: HttpBody::empty(),
        })?;
        if response.status != 200 {
            return Err(format!(
                "Drive download for {file_id} returned HTTP {}",
                response.status
            ));
        }
        Ok(response.body)
    }

    fn file_revision(&self, file_id: &str) -> Result<DriveFileRevision, String> {
        let response = self.transport.execute(HttpRequest {
            method: "GET".to_owned(),
            url: format!(
                "{}/files/{}?fields={}",
                self.api_base,
                encode(file_id),
                encode(Self::file_fields())
            ),
            headers: self.headers()?,
            body: HttpBody::empty(),
        })?;
        if response.status != 200 {
            return Err(format!(
                "Drive files.get for {file_id} returned HTTP {}",
                response.status
            ));
        }
        let file: DriveFileWire =
            serde_json::from_slice(&response.body).map_err(|error| error.to_string())?;
        file.into_revision()
    }

    fn find_delivery(&self, delivery_id: &str) -> Result<Vec<DriveFileRevision>, String> {
        if !delivery_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        {
            return Err("Drive delivery ID is invalid".to_owned());
        }
        let query = format!(
            "appProperties has {{ key='inkbridgeDeliveryId' and value='{delivery_id}' }} and trashed=false"
        );
        let fields = format!("files({})", Self::file_fields());
        let response = self.transport.execute(HttpRequest {
            method: "GET".to_owned(),
            url: format!(
                "{}/files?q={}&spaces=drive&fields={}",
                self.api_base,
                encode(&query),
                encode(&fields)
            ),
            headers: self.headers()?,
            body: HttpBody::empty(),
        })?;
        if response.status != 200 {
            return Err(format!(
                "Drive files.list returned HTTP {}",
                response.status
            ));
        }
        let wire: DriveFilesWire =
            serde_json::from_slice(&response.body).map_err(|error| error.to_string())?;
        wire.files
            .into_iter()
            .map(DriveFileWire::into_revision)
            .collect()
    }

    fn create_delivery(
        &self,
        output: &PreparedDriveOutput,
        bytes: &[u8],
    ) -> Result<DriveFileRevision, String> {
        let mime_type = if output.file_name.ends_with(".pdf") {
            "application/pdf"
        } else {
            "application/json"
        };
        let metadata = serde_json::to_vec(&json!({
            "name": output.file_name,
            "parents": [output.parent_folder_id],
            "appProperties": output.app_properties,
        }))
        .map_err(|error| error.to_string())?;
        let mut headers = self.headers()?;
        headers.insert("Content-Type".to_owned(), "application/json".to_owned());
        headers.insert("X-Upload-Content-Type".to_owned(), mime_type.to_owned());
        headers.insert(
            "X-Upload-Content-Length".to_owned(),
            bytes.len().to_string(),
        );
        let session = self.transport.execute(HttpRequest {
            method: "POST".to_owned(),
            url: format!(
                "{}/files?uploadType=resumable&fields={}",
                self.upload_base,
                encode(Self::file_fields())
            ),
            headers,
            body: HttpBody::bytes(metadata),
        })?;
        if !matches!(session.status, 200 | 201) {
            return Err(format!(
                "Drive resumable create session returned HTTP {}",
                session.status
            ));
        }
        let upload_url = session
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("location"))
            .map(|(_, value)| value.clone())
            .ok_or_else(|| "Drive resumable create returned no Location header".to_owned())?;
        let response = self.transport.execute(HttpRequest {
            method: "PUT".to_owned(),
            url: upload_url,
            headers: BTreeMap::from([
                ("Content-Type".to_owned(), mime_type.to_owned()),
                ("Content-Length".to_owned(), bytes.len().to_string()),
            ]),
            body: HttpBody::bytes(bytes.to_vec()),
        })?;
        if !matches!(response.status, 200 | 201) {
            return Err(format!(
                "Drive file create returned HTTP {}",
                response.status
            ));
        }
        let file: DriveFileWire =
            serde_json::from_slice(&response.body).map_err(|error| error.to_string())?;
        file.into_revision()
    }
}

#[derive(Clone)]
pub struct FirestoreGatewayCheckpointStore {
    project_id: String,
    database_id: String,
    checkpoint_id: String,
    api_base: String,
    transport: Arc<dyn HttpTransport>,
    tokens: Arc<dyn TokenProvider>,
}

impl FirestoreGatewayCheckpointStore {
    pub fn new(
        project_id: impl Into<String>,
        database_id: impl Into<String>,
        checkpoint_id: impl Into<String>,
        transport: Arc<dyn HttpTransport>,
        tokens: Arc<dyn TokenProvider>,
    ) -> Self {
        Self {
            project_id: project_id.into(),
            database_id: database_id.into(),
            checkpoint_id: checkpoint_id.into(),
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

    fn root(&self) -> String {
        format!(
            "{}/projects/{}/databases/{}/documents",
            self.api_base,
            encode(&self.project_id),
            encode(&self.database_id)
        )
    }

    fn document_name(&self) -> String {
        format!(
            "projects/{}/databases/{}/documents/inkbridgeDriveGateways/{}",
            self.project_id, self.database_id, self.checkpoint_id
        )
    }

    fn headers(&self) -> Result<BTreeMap<String, String>, String> {
        let mut headers = bearer_headers(&self.tokens.access_token()?);
        headers.insert("Content-Type".to_owned(), "application/json".to_owned());
        Ok(headers)
    }
}

impl CheckpointStore for FirestoreGatewayCheckpointStore {
    fn load(&self) -> Result<VersionedCheckpoint, String> {
        let response = self.transport.execute(HttpRequest {
            method: "GET".to_owned(),
            url: format!(
                "{}/inkbridgeDriveGateways/{}",
                self.root(),
                encode(&self.checkpoint_id)
            ),
            headers: self.headers()?,
            body: HttpBody::empty(),
        })?;
        if response.status == 404 {
            return Ok(VersionedCheckpoint {
                value: DriveGatewayCheckpoint::empty(),
                version: None,
            });
        }
        if response.status != 200 {
            return Err(format!(
                "Firestore Drive checkpoint read returned HTTP {}",
                response.status
            ));
        }
        let value: Value =
            serde_json::from_slice(&response.body).map_err(|error| error.to_string())?;
        let version = value
            .get("updateTime")
            .and_then(Value::as_str)
            .ok_or_else(|| "Firestore Drive checkpoint has no updateTime".to_owned())?
            .to_owned();
        let encoded = value
            .pointer("/fields/checkpoint/bytesValue")
            .and_then(Value::as_str)
            .ok_or_else(|| "Firestore Drive checkpoint has no payload".to_owned())?;
        let bytes = BASE64.decode(encoded).map_err(|error| error.to_string())?;
        let checkpoint: DriveGatewayCheckpoint =
            serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        checkpoint.validate()?;
        Ok(VersionedCheckpoint {
            value: checkpoint,
            version: Some(version),
        })
    }

    fn compare_and_swap(
        &self,
        expected_version: Option<&str>,
        value: &DriveGatewayCheckpoint,
    ) -> Result<VersionedCheckpoint, String> {
        value.validate()?;
        let payload = serde_json::to_vec(value).map_err(|error| error.to_string())?;
        let precondition = expected_version.map_or_else(
            || json!({"exists": false}),
            |version| json!({"updateTime": version}),
        );
        let body = serde_json::to_vec(&json!({
            "writes": [{
                "update": {
                    "name": self.document_name(),
                    "fields": {"checkpoint": {"bytesValue": BASE64.encode(payload)}}
                },
                "currentDocument": precondition
            }]
        }))
        .map_err(|error| error.to_string())?;
        let response = self.transport.execute(HttpRequest {
            method: "POST".to_owned(),
            url: format!(
                "{}/projects/{}/databases/{}/documents:commit",
                self.api_base,
                encode(&self.project_id),
                encode(&self.database_id)
            ),
            headers: self.headers()?,
            body: HttpBody::bytes(body),
        })?;
        let stale = matches!(response.status, 409 | 412)
            || serde_json::from_slice::<Value>(&response.body)
                .ok()
                .and_then(|body| {
                    body.pointer("/error/status")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .is_some_and(|status| matches!(status.as_str(), "ABORTED" | "FAILED_PRECONDITION"));
        if stale {
            return Err("Drive checkpoint generation changed; retry the job".to_owned());
        }
        if response.status != 200 {
            return Err(format!(
                "Firestore Drive checkpoint compare-and-swap returned HTTP {}",
                response.status
            ));
        }
        let committed: Value =
            serde_json::from_slice(&response.body).map_err(|error| error.to_string())?;
        let version = committed
            .pointer("/writeResults/0/updateTime")
            .and_then(Value::as_str)
            .or_else(|| committed.get("commitTime").and_then(Value::as_str))
            .ok_or_else(|| "Firestore checkpoint commit returned no update time".to_owned())?
            .to_owned();
        Ok(VersionedCheckpoint {
            value: value.clone(),
            version: Some(version),
        })
    }
}

#[derive(Clone)]
pub struct FirestoreOnboardingApprovalStore {
    project_id: String,
    database_id: String,
    api_base: String,
    transport: Arc<dyn HttpTransport>,
    tokens: Arc<dyn TokenProvider>,
}

impl FirestoreOnboardingApprovalStore {
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

    fn root(&self) -> String {
        format!(
            "{}/projects/{}/databases/{}/documents",
            self.api_base,
            encode(&self.project_id),
            encode(&self.database_id)
        )
    }

    fn headers(&self) -> Result<BTreeMap<String, String>, String> {
        Ok(bearer_headers(&self.tokens.access_token()?))
    }
}

impl OnboardingApprovalStore for FirestoreOnboardingApprovalStore {
    fn load(&self, drive_file_id: &str) -> Result<Option<OnboardingApproval>, String> {
        let response = self.transport.execute(HttpRequest {
            method: "GET".to_owned(),
            url: format!(
                "{}/inkbridgeDriveApprovals/{}",
                self.root(),
                encode(drive_file_id)
            ),
            headers: self.headers()?,
            body: HttpBody::empty(),
        })?;
        if response.status == 404 {
            return Ok(None);
        }
        if response.status != 200 {
            return Err(format!(
                "Firestore Drive approval read returned HTTP {}",
                response.status
            ));
        }
        let value: Value =
            serde_json::from_slice(&response.body).map_err(|error| error.to_string())?;
        let encoded = value
            .pointer("/fields/approval/bytesValue")
            .and_then(Value::as_str)
            .ok_or_else(|| "Firestore Drive approval has no payload".to_owned())?;
        let bytes = BASE64.decode(encoded).map_err(|error| error.to_string())?;
        let approval: OnboardingApproval =
            serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        let approved_file_id = match &approval {
            OnboardingApproval::Original { approval } => &approval.drive_file_id,
            OnboardingApproval::DeviceArtifact { approval } => &approval.drive_file_id,
        };
        if approved_file_id != drive_file_id {
            return Err(format!(
                "Firestore Drive approval document {drive_file_id} names a different file"
            ));
        }
        Ok(Some(approval))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use inkbridge_cloud_runtime::{HttpResponse, StaticTokenProvider};
    use std::collections::VecDeque;

    #[derive(Default)]
    struct RecordingTransport {
        requests: Mutex<Vec<HttpRequest>>,
        responses: Mutex<VecDeque<HttpResponse>>,
    }

    impl RecordingTransport {
        fn push(&self, status: u16, body: &[u8]) {
            self.responses.lock().unwrap().push_back(HttpResponse {
                status,
                headers: BTreeMap::new(),
                body: body.to_vec(),
            });
        }

        fn push_with_headers(&self, status: u16, headers: BTreeMap<String, String>, body: &[u8]) {
            self.responses.lock().unwrap().push_back(HttpResponse {
                status,
                headers,
                body: body.to_vec(),
            });
        }
    }

    impl HttpTransport for RecordingTransport {
        fn execute(&self, request: HttpRequest) -> Result<HttpResponse, String> {
            self.requests.lock().unwrap().push(request);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "missing fake response".to_owned())
        }
    }

    #[test]
    fn secret_manager_decodes_payload_without_logging_secret_bytes() {
        let transport = Arc::new(RecordingTransport::default());
        transport.push(200, br#"{"payload":{"data":"c2VjcmV0"}}"#);
        let manager = GoogleSecretManager::new(
            "project",
            transport.clone(),
            Arc::new(StaticTokenProvider("gcp-token".to_owned())),
        )
        .with_api_base("https://secret.example/v1");

        assert_eq!(manager.access_latest("drive-refresh").unwrap(), b"secret");
        let requests = transport.requests.lock().unwrap();
        assert!(requests[0]
            .url
            .ends_with("/projects/project/secrets/drive%2Drefresh/versions/latest:access"));
        assert_eq!(requests[0].headers["Authorization"], "Bearer gcp-token");
    }

    #[test]
    fn drive_oauth_refresh_is_url_encoded_and_cached() {
        let transport = Arc::new(RecordingTransport::default());
        transport.push(200, br#"{"access_token":"drive-token","expires_in":3600}"#);
        let provider = DriveOAuthTokenProvider::from_bytes(
            br#"{"installed":{"client_id":"client id","client_secret":"secret+value","token_uri":"https://oauth.example/token"}}"#.to_vec(),
            b"refresh/value".to_vec(),
            transport.clone(),
        );

        assert_eq!(provider.access_token().unwrap(), "drive-token");
        assert_eq!(provider.access_token().unwrap(), "drive-token");
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url, "https://oauth.example/token");
        let body = String::from_utf8_lossy(requests[0].body.as_ref());
        assert!(body.contains("client_id=client%20id"));
        assert!(body.contains("client_secret=secret%2Bvalue"));
        assert!(body.contains("refresh_token=refresh%2Fvalue"));
    }

    #[test]
    fn drive_changes_preserve_file_versions_and_removed_records() {
        let transport = Arc::new(RecordingTransport::default());
        transport.push(
            200,
            br#"{
              "newStartPageToken":"next",
              "changes":[
                {"fileId":"file-1","removed":false,"file":{
                  "id":"file-1","name":"book.pdf","version":"7",
                  "mimeType":"application/pdf","parents":["boox-folder"],
                  "size":"123","trashed":false,"appProperties":{}
                }},
                {"fileId":"file-2","removed":true}
              ]
            }"#,
        );
        let api = GoogleDriveApi::new(
            transport.clone(),
            Arc::new(StaticTokenProvider("drive-token".to_owned())),
        )
        .with_endpoints("https://drive.example/v3", "https://upload.example/v3");

        let page = api.list_changes("cursor/1").unwrap();
        assert_eq!(page.new_start_page_token.as_deref(), Some("next"));
        assert_eq!(page.changes[0].file.version, 7);
        assert_eq!(page.changes[0].file.size, 123);
        assert!(page.changes[1].file.trashed);
        let requests = transport.requests.lock().unwrap();
        assert!(requests[0].url.contains("pageToken=cursor%2F1"));
        assert!(requests[0].url.contains("includeRemoved=true"));
    }

    #[test]
    fn drive_initial_snapshot_paginates_both_device_folders() {
        let transport = Arc::new(RecordingTransport::default());
        transport.push(
            200,
            br#"{
              "nextPageToken":"boox-next",
              "files":[{"id":"boox-1","name":"book.pdf","version":"1",
                "mimeType":"application/pdf","parents":["boox-folder"],
                "size":"12","trashed":false,"appProperties":{}}]
            }"#,
        );
        transport.push(
            200,
            br#"{"files":[{"id":"boox-2","name":"notes.json","version":"2",
              "mimeType":"application/json","parents":["boox-folder"],
              "size":"8","trashed":false,"appProperties":{}}]}"#,
        );
        transport.push(200, br#"{"files":[]}"#);
        let api = GoogleDriveApi::new(
            transport.clone(),
            Arc::new(StaticTokenProvider("drive-token".to_owned())),
        )
        .with_endpoints("https://drive.example/v3", "https://upload.example/v3");

        let files = api
            .list_initial_files(&["boox-folder".to_owned(), "supernote-folder".to_owned()])
            .unwrap();
        assert_eq!(files.len(), 2);
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(requests[0].url.contains("boox%2Dfolder"));
        assert!(requests[1].url.contains("pageToken=boox%2Dnext"));
        assert!(requests[2].url.contains("supernote%2Dfolder"));
    }

    #[test]
    fn drive_file_revision_refetches_exact_metadata_after_download() {
        let transport = Arc::new(RecordingTransport::default());
        transport.push(
            200,
            br#"{"id":"file-1","name":"book.pdf","version":"8",
              "mimeType":"application/pdf","parents":["boox-folder"],
              "size":"123","trashed":false,"appProperties":{}}"#,
        );
        let api = GoogleDriveApi::new(
            transport.clone(),
            Arc::new(StaticTokenProvider("drive-token".to_owned())),
        )
        .with_endpoints("https://drive.example/v3", "https://upload.example/v3");

        let revision = api.file_revision("file-1").unwrap();
        assert_eq!(revision.version, 8);
        assert_eq!(revision.size, 123);
        let requests = transport.requests.lock().unwrap();
        assert!(requests[0].url.contains("/files/file%2D1?fields="));
        assert!(!requests[0].url.contains("alt=media"));
    }

    #[test]
    fn drive_create_uses_resumable_upload_and_private_delivery_properties() {
        let transport = Arc::new(RecordingTransport::default());
        transport.push_with_headers(
            200,
            BTreeMap::from([(
                "Location".to_owned(),
                "https://upload.example/session".to_owned(),
            )]),
            b"",
        );
        transport.push(
            200,
            br#"{
              "id":"created","name":"delivery.json","version":"4",
              "mimeType":"application/json","parents":["sn-folder"],
              "size":"3","trashed":false,
              "appProperties":{"inkbridgeDeliveryId":"delivery-1"}
            }"#,
        );
        let api = GoogleDriveApi::new(
            transport.clone(),
            Arc::new(StaticTokenProvider("drive-token".to_owned())),
        )
        .with_endpoints("https://drive.example/v3", "https://upload.example/v3");
        let output = PreparedDriveOutput {
            delivery_id: "delivery-1".to_owned(),
            gcs_object_path: "Supernote_Folder/doc/view.json".to_owned(),
            gcs_generation: 5,
            document_id:
                "inkbridge-doc-v1-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_owned(),
            target: inkbridge_broker::DeviceSide::Supernote,
            content_sha256: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                .to_owned(),
            source_revisions: inkbridge_broker::RevisionPair::default(),
            parent_folder_id: "sn-folder".to_owned(),
            file_name: "delivery.json".to_owned(),
            app_properties: BTreeMap::from([(
                "inkbridgeDeliveryId".to_owned(),
                "delivery-1".to_owned(),
            )]),
        };

        let created = api.create_delivery(&output, b"new").unwrap();
        assert_eq!(created.file_id, "created");
        let requests = transport.requests.lock().unwrap();
        assert!(requests[0].url.contains("uploadType=resumable"));
        assert!(String::from_utf8_lossy(requests[0].body.as_ref()).contains("inkbridgeDeliveryId"));
        assert_eq!(requests[1].url, "https://upload.example/session");
        assert_eq!(requests[1].body.as_ref(), b"new");
    }

    #[test]
    fn firestore_checkpoint_commit_uses_exact_update_time_precondition() {
        let transport = Arc::new(RecordingTransport::default());
        transport.push(
            200,
            br#"{"writeResults":[{"updateTime":"2026-08-30T12:00:01Z"}]}"#,
        );
        let store = FirestoreGatewayCheckpointStore::new(
            "project",
            "(default)",
            "primary",
            transport.clone(),
            Arc::new(StaticTokenProvider("gcp-token".to_owned())),
        )
        .with_api_base("https://firestore.example/v1");
        let checkpoint = DriveGatewayCheckpoint::empty();

        let committed = store
            .compare_and_swap(Some("2026-08-30T12:00:00Z"), &checkpoint)
            .unwrap();
        assert_eq!(committed.version.as_deref(), Some("2026-08-30T12:00:01Z"));
        let requests = transport.requests.lock().unwrap();
        let body: Value = serde_json::from_slice(requests[0].body.as_ref()).unwrap();
        assert_eq!(
            body.pointer("/writes/0/currentDocument/updateTime")
                .and_then(Value::as_str),
            Some("2026-08-30T12:00:00Z")
        );
    }

    #[test]
    fn firestore_checkpoint_rejects_stale_generation() {
        let transport = Arc::new(RecordingTransport::default());
        transport.push(409, br#"{"error":{"status":"ABORTED"}}"#);
        let store = FirestoreGatewayCheckpointStore::new(
            "project",
            "(default)",
            "primary",
            transport,
            Arc::new(StaticTokenProvider("gcp-token".to_owned())),
        )
        .with_api_base("https://firestore.example/v1");

        let error = store
            .compare_and_swap(Some("old"), &DriveGatewayCheckpoint::empty())
            .unwrap_err();
        assert!(error.contains("generation changed"));
    }

    #[test]
    fn firestore_onboarding_approval_is_exact_and_file_bound() {
        let transport = Arc::new(RecordingTransport::default());
        let approval = OnboardingApproval::Original {
            approval: inkbridge_drive_gateway::OriginalRegistrationApproval {
                drive_file_id: "boox-file".to_owned(),
                drive_file_version: 7,
                content_sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_owned(),
            },
        };
        let encoded = BASE64.encode(serde_json::to_vec(&approval).unwrap());
        transport.push(
            200,
            serde_json::to_vec(&json!({
                "fields": {"approval": {"bytesValue": encoded}}
            }))
            .unwrap()
            .as_slice(),
        );
        let store = FirestoreOnboardingApprovalStore::new(
            "project",
            "(default)",
            transport.clone(),
            Arc::new(StaticTokenProvider("gcp-token".to_owned())),
        )
        .with_api_base("https://firestore.example/v1");

        assert_eq!(store.load("boox-file").unwrap(), Some(approval));
        let requests = transport.requests.lock().unwrap();
        assert!(requests[0]
            .url
            .ends_with("/inkbridgeDriveApprovals/boox%2Dfile"));
    }
}
