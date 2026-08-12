use inkbridge_cloud_runtime::{
    FirestoreCanonicalStateStore, GoogleCloudStorage, GoogleTokenProvider, ReqwestTransport,
    RuntimeService,
};
use std::env;
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("inkbridge-cloud-runtime: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let project_id = required_env("INKBRIDGE_GCP_PROJECT")?;
    let bucket = required_env("INKBRIDGE_GCS_BUCKET")?;
    let database =
        env::var("INKBRIDGE_FIRESTORE_DATABASE").unwrap_or_else(|_| "(default)".to_owned());
    let port = env::var("PORT")
        .unwrap_or_else(|_| "8080".to_owned())
        .parse::<u16>()
        .map_err(|error| format!("invalid PORT: {error}"))?;
    let transport = Arc::new(ReqwestTransport::new(Duration::from_secs(840))?);
    let tokens = Arc::new(GoogleTokenProvider::new(transport.clone()));
    let objects = Arc::new(GoogleCloudStorage::new(
        &bucket,
        transport.clone(),
        tokens.clone(),
    ));
    let states = Arc::new(FirestoreCanonicalStateStore::new(
        project_id, database, transport, tokens,
    ));
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .map_err(|error| error.to_string())?;
    axum::serve(
        listener,
        RuntimeService::new(bucket, objects, states).router(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .map_err(|error| error.to_string())
}

fn required_env(name: &str) -> Result<String, String> {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{name} is required"))
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
