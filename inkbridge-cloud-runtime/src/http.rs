use reqwest::blocking::Client;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::env;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

pub trait HttpTransport: Send + Sync {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, String>;
}

#[derive(Clone)]
pub struct ReqwestTransport {
    client: Client,
}

impl ReqwestTransport {
    pub fn new(timeout: Duration) -> Result<Self, String> {
        Client::builder()
            .timeout(timeout)
            .build()
            .map(|client| Self { client })
            .map_err(|error| error.to_string())
    }
}

impl HttpTransport for ReqwestTransport {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, String> {
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|error| error.to_string())?;
        let mut builder = self.client.request(method, &request.url);
        for (name, value) in request.headers {
            builder = builder.header(name, value);
        }
        let response = builder
            .body(request.body)
            .send()
            .map_err(|error| error.to_string())?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.as_str().to_owned(), value.to_owned()))
            })
            .collect();
        let body = response
            .bytes()
            .map_err(|error| error.to_string())?
            .to_vec();
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

pub trait TokenProvider: Send + Sync {
    fn access_token(&self) -> Result<String, String>;
}

#[derive(Clone)]
pub struct GoogleTokenProvider {
    transport: Arc<dyn HttpTransport>,
    cached: Arc<Mutex<Option<CachedToken>>>,
}

#[derive(Clone)]
struct CachedToken {
    value: String,
    refresh_at: Instant,
}

#[derive(Deserialize)]
struct MetadataToken {
    access_token: String,
    expires_in: u64,
}

impl GoogleTokenProvider {
    pub fn new(transport: Arc<dyn HttpTransport>) -> Self {
        Self {
            transport,
            cached: Arc::new(Mutex::new(None)),
        }
    }
}

impl TokenProvider for GoogleTokenProvider {
    fn access_token(&self) -> Result<String, String> {
        if let Ok(value) = env::var("INKBRIDGE_GOOGLE_ACCESS_TOKEN") {
            if !value.trim().is_empty() {
                return Ok(value);
            }
        }
        if let Some(cached) = self
            .cached
            .lock()
            .map_err(|_| "token cache lock was poisoned".to_owned())?
            .as_ref()
            .filter(|token| token.refresh_at > Instant::now())
        {
            return Ok(cached.value.clone());
        }
        let response = self.transport.execute(HttpRequest {
            method: "GET".to_owned(),
            url: "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token".to_owned(),
            headers: BTreeMap::from([("Metadata-Flavor".to_owned(), "Google".to_owned())]),
            body: Vec::new(),
        })?;
        if response.status != 200 {
            return Err(format!(
                "Google metadata token endpoint returned HTTP {}",
                response.status
            ));
        }
        let token: MetadataToken =
            serde_json::from_slice(&response.body).map_err(|error| error.to_string())?;
        let refresh_after = token.expires_in.saturating_sub(60);
        let cached = CachedToken {
            value: token.access_token.clone(),
            refresh_at: Instant::now() + Duration::from_secs(refresh_after),
        };
        *self
            .cached
            .lock()
            .map_err(|_| "token cache lock was poisoned".to_owned())? = Some(cached);
        Ok(token.access_token)
    }
}

#[derive(Clone)]
pub struct StaticTokenProvider(pub String);

impl TokenProvider for StaticTokenProvider {
    fn access_token(&self) -> Result<String, String> {
        Ok(self.0.clone())
    }
}

pub fn bearer_headers(token: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("Authorization".to_owned(), format!("Bearer {token}"))])
}
