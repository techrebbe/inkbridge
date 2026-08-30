use inkbridge_cloud_runtime::{
    FirestoreCanonicalStateStore, GoogleCloudStorage, GoogleTokenProvider, ReqwestTransport,
};
use inkbridge_drive_gateway::{DriveGatewayConfig, DRIVE_GATEWAY_SCHEMA_VERSION};
use inkbridge_drive_runtime::{
    CloudBrokerPort, CloudEvidenceStore, DriveOAuthTokenProvider, FirestoreGatewayCheckpointStore,
    GatewayJob, GoogleDriveApi, GoogleSecretManager, RunMode,
};
use std::env;
use std::sync::Arc;
use std::time::Duration;

fn main() {
    if let Err(error) = run() {
        eprintln!("inkbridge-drive-runtime: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let project_id = required_env("INKBRIDGE_GCP_PROJECT")?;
    let bucket = required_env("INKBRIDGE_GCS_BUCKET")?;
    let database =
        env::var("INKBRIDGE_FIRESTORE_DATABASE").unwrap_or_else(|_| "(default)".to_owned());
    let boox_folder_id = required_env("INKBRIDGE_DRIVE_BOOX_FOLDER_ID")?;
    let supernote_folder_id = required_env("INKBRIDGE_DRIVE_SUPERNOTE_FOLDER_ID")?;
    let oauth_client_secret = required_env("INKBRIDGE_DRIVE_OAUTH_CLIENT_SECRET")?;
    let refresh_token_secret = required_env("INKBRIDGE_DRIVE_REFRESH_TOKEN_SECRET")?;
    let checkpoint_id =
        env::var("INKBRIDGE_DRIVE_CHECKPOINT_ID").unwrap_or_else(|_| "primary".to_owned());
    let mode = if env::args().any(|argument| argument == "--apply") {
        RunMode::Apply
    } else {
        RunMode::DryRun
    };

    let transport = Arc::new(ReqwestTransport::new(Duration::from_secs(840))?);
    let gcp_tokens = Arc::new(GoogleTokenProvider::new(transport.clone()));
    let secrets = GoogleSecretManager::new(&project_id, transport.clone(), gcp_tokens.clone());
    let drive_tokens = Arc::new(DriveOAuthTokenProvider::from_secret_manager(
        &secrets,
        &oauth_client_secret,
        &refresh_token_secret,
        transport.clone(),
    )?);
    let drive = Arc::new(GoogleDriveApi::new(transport.clone(), drive_tokens));
    let objects = Arc::new(GoogleCloudStorage::new(
        &bucket,
        transport.clone(),
        gcp_tokens.clone(),
    ));
    let states = Arc::new(FirestoreCanonicalStateStore::new(
        &project_id,
        &database,
        transport.clone(),
        gcp_tokens.clone(),
    ));
    let checkpoints = Arc::new(FirestoreGatewayCheckpointStore::new(
        &project_id,
        &database,
        checkpoint_id,
        transport,
        gcp_tokens,
    ));
    let evidence = Arc::new(CloudEvidenceStore::new(objects.clone()));
    let broker = Arc::new(CloudBrokerPort::new(bucket, objects, states));
    let job = GatewayJob::new(
        DriveGatewayConfig {
            schema_version: DRIVE_GATEWAY_SCHEMA_VERSION,
            boox_folder_id,
            supernote_folder_id,
        },
        drive,
        checkpoints,
        evidence,
        broker,
    )?;
    let report = job.run_once(mode)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn required_env(name: &str) -> Result<String, String> {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{name} is required"))
}
