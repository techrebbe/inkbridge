use crate::transport::first_unacknowledged_supernote_delivery;
use crate::{DocumentFolders, TransportState};
use inkbridge_broker::RevisionPair;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const COMPANION_STATUS_SCHEMA_VERSION: u32 = 1;
pub const COMPANION_STATUS_FILE: &str = "inkbridge-status.json";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompanionSyncStatus {
    Synced,
    Pending,
    Conflict,
    Error,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CompanionStatus {
    pub schema_version: u32,
    pub document_id: String,
    pub status: CompanionSyncStatus,
    pub revisions: RevisionPair,
    pub boox_pending: bool,
    pub supernote_pending: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub supernote_delivery_pending: bool,
    pub supernote_accepted_content_sha256: BTreeSet<String>,
    /// Hashes of the exact native export bytes accepted by the broker.
    ///
    /// `supernote_accepted_content_sha256` identifies the possibly rebased
    /// broker payload retained as a baseline.  The companion needs this
    /// source-view hash set before it can safely replace a same-page outgoing
    /// file during an ordinary-PDF/Virtual-Spread representation switch.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub supernote_accepted_source_view_sha256: BTreeSet<String>,
    pub conflict_count: usize,
    pub updated_at_unix_millis: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl CompanionStatus {
    pub fn from_state(
        document: &DocumentFolders,
        state: &TransportState,
        now: SystemTime,
        error: Option<&str>,
    ) -> Result<Self, String> {
        let current = state
            .documents
            .get(&document.document_id)
            .cloned()
            .unwrap_or_default();
        let (supernote_delivery_pending, delivery_error) =
            match first_unacknowledged_supernote_delivery(document) {
                Ok(delivery) => (delivery.is_some(), None),
                Err(message) => (false, Some(message)),
            };
        let effective_error = error.map(str::to_owned).or(delivery_error);
        let supernote_pending = current.supernote.pending.is_some() || supernote_delivery_pending;
        let status = if effective_error.is_some() {
            CompanionSyncStatus::Error
        } else if !current.conflicts.is_empty() {
            CompanionSyncStatus::Conflict
        } else if current.boox.pending.is_some() || supernote_pending {
            CompanionSyncStatus::Pending
        } else {
            CompanionSyncStatus::Synced
        };
        let pending_source_view = current
            .supernote
            .pending
            .as_ref()
            .map(|pending| pending.local_content_sha256.as_str());
        Ok(Self {
            schema_version: COMPANION_STATUS_SCHEMA_VERSION,
            document_id: document.document_id.clone(),
            status,
            revisions: current.revisions,
            boox_pending: current.boox.pending.is_some(),
            supernote_pending,
            supernote_delivery_pending,
            supernote_accepted_content_sha256: current
                .supernote
                .accepted_local_hashes
                .values()
                .cloned()
                .collect(),
            supernote_accepted_source_view_sha256: current
                .supernote
                .uploaded_local_hashes
                .values()
                .filter(|hash| Some(hash.as_str()) != pending_source_view)
                .cloned()
                .collect(),
            conflict_count: current.conflicts.len(),
            updated_at_unix_millis: now
                .duration_since(UNIX_EPOCH)
                .map_err(|_| "status timestamp predates the Unix epoch".to_owned())?
                .as_millis() as u64,
            message: effective_error.map(|message| message.chars().take(4096).collect()),
        })
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

pub fn publish_companion_status(
    document: &DocumentFolders,
    state: &TransportState,
    now: SystemTime,
    error: Option<&str>,
) -> Result<PathBuf, String> {
    let status = CompanionStatus::from_state(document, state, now, error)?;
    let mut bytes = serde_json::to_vec_pretty(&status).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    let destination = document
        .supernote_incoming_directory
        .join(COMPANION_STATUS_FILE);
    publish_status_bytes(&destination, &bytes)?;
    Ok(destination)
}

fn publish_status_bytes(destination: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    let next = destination.with_extension("json.next");
    let previous = destination.with_extension("json.previous");
    reconcile_status_files(destination, &next, &previous)?;

    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&next)
        .map_err(|error| format!("could not write {}: {error}", next.display()))?;
    output
        .write_all(bytes)
        .and_then(|_| output.flush())
        .and_then(|_| output.sync_all())
        .map_err(|error| format!("could not finalize {}: {error}", next.display()))?;
    drop(output);

    let had_current = regular_file_exists(destination)?;
    if had_current {
        remove_file_if_exists(&previous)?;
        fs::rename(destination, &previous).map_err(|error| {
            format!(
                "could not stage companion status {} as {}: {error}",
                destination.display(),
                previous.display()
            )
        })?;
    }
    if let Err(error) = fs::rename(&next, destination) {
        if had_current {
            let _ = fs::rename(&previous, destination);
        }
        return Err(format!(
            "could not publish companion status {}: {error}",
            destination.display()
        ));
    }
    remove_file_if_exists(&previous)
}

fn reconcile_status_files(destination: &Path, next: &Path, previous: &Path) -> Result<(), String> {
    if !regular_file_exists(destination)? && regular_file_exists(previous)? {
        fs::rename(previous, destination).map_err(|error| {
            format!(
                "could not restore interrupted companion status {}: {error}",
                destination.display()
            )
        })?;
    }
    if regular_file_exists(destination)? {
        remove_file_if_exists(previous)?;
    }
    remove_file_if_exists(next)
}

fn regular_file_exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(format!("{} must not be a symlink", path.display()))
        }
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(format!("{} is not a regular file", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("could not inspect {}: {error}", path.display())),
    }
}

fn remove_file_if_exists(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("could not remove {}: {error}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DocumentTransportState, PendingUpload, SideTransportState, TransportState};
    use tempfile::tempdir;

    fn document(root: &Path) -> DocumentFolders {
        DocumentFolders {
            document_id: format!("inkbridge-doc-v1-{}", "a".repeat(64)),
            original_file_name: "Example.pdf".to_owned(),
            boox_pdf: root.join("boox/Example.pdf"),
            supernote_export_directory: root.join("supernote/outgoing"),
            supernote_incoming_directory: root.join("supernote/incoming"),
        }
    }

    fn pending() -> PendingUpload {
        PendingUpload {
            object_path: "Supernote_Folder/doc/uploads/r1.json".to_owned(),
            generation: 1,
            source_revision: 1,
            based_on: RevisionPair::default(),
            local_path: "page-0001.json".to_owned(),
            local_content_sha256: "a".repeat(64),
            payload_content_sha256: "b".repeat(64),
        }
    }

    #[test]
    fn reports_pending_synced_conflict_and_error_states() {
        let root = tempdir().unwrap();
        let document = document(root.path());
        let mut state = TransportState::empty();
        state.documents.insert(
            document.document_id.clone(),
            DocumentTransportState {
                supernote: SideTransportState {
                    pending: Some(pending()),
                    uploaded_local_hashes: [
                        ("accepted-page.json".to_owned(), "d".repeat(64)),
                        ("pending-page.json".to_owned(), "a".repeat(64)),
                    ]
                    .into_iter()
                    .collect(),
                    accepted_local_hashes: [("page-0001.json".to_owned(), "c".repeat(64))]
                        .into_iter()
                        .collect(),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let now = UNIX_EPOCH + std::time::Duration::from_secs(5);
        let pending = CompanionStatus::from_state(&document, &state, now, None).unwrap();
        assert_eq!(pending.status, CompanionSyncStatus::Pending);
        assert_eq!(
            pending.supernote_accepted_content_sha256,
            ["c".repeat(64)].into_iter().collect()
        );
        assert_eq!(
            pending.supernote_accepted_source_view_sha256,
            ["d".repeat(64)].into_iter().collect()
        );

        state
            .documents
            .get_mut(&document.document_id)
            .unwrap()
            .supernote
            .pending = None;
        let synced = CompanionStatus::from_state(&document, &state, now, None).unwrap();
        assert_eq!(synced.status, CompanionSyncStatus::Synced);
        assert_eq!(
            synced.supernote_accepted_source_view_sha256,
            ["a".repeat(64), "d".repeat(64)].into_iter().collect()
        );

        state
            .documents
            .get_mut(&document.document_id)
            .unwrap()
            .conflicts
            .insert("Conflicts/doc/event#1".to_owned());
        let conflict = CompanionStatus::from_state(&document, &state, now, None).unwrap();
        assert_eq!(conflict.status, CompanionSyncStatus::Conflict);

        let error =
            CompanionStatus::from_state(&document, &state, now, Some("cloud failed")).unwrap();
        assert_eq!(error.status, CompanionSyncStatus::Error);
        assert_eq!(error.message.as_deref(), Some("cloud failed"));
    }

    #[test]
    fn downloaded_manifest_remains_pending_until_its_acknowledgement_is_valid() {
        let root = tempdir().unwrap();
        let document = document(root.path());
        fs::create_dir_all(&document.supernote_incoming_directory).unwrap();
        let manifest = br#"{"schemaVersion":1,"manifestId":"incoming","operations":[]}"#;
        fs::write(
            document
                .supernote_incoming_directory
                .join("r00000000000000000001-r00000000000000000000-g1.operations.json"),
            manifest,
        )
        .unwrap();
        let state = TransportState::empty();

        let pending = CompanionStatus::from_state(&document, &state, UNIX_EPOCH, None).unwrap();
        assert_eq!(pending.status, CompanionSyncStatus::Pending);
        assert!(pending.supernote_pending);
        assert!(pending.supernote_delivery_pending);

        let delivery_id = inkbridge_broker::sha256_hex(manifest);
        let acknowledged = document.supernote_acknowledged_directory();
        fs::create_dir_all(&acknowledged).unwrap();
        let acknowledgement = acknowledged.join(format!("{delivery_id}.ack.json"));
        fs::write(&acknowledgement, b"{}\n").unwrap();
        let invalid = CompanionStatus::from_state(&document, &state, UNIX_EPOCH, None).unwrap();
        assert_eq!(invalid.status, CompanionSyncStatus::Error);
        assert!(invalid.message.as_deref().is_some_and(|message| {
            message.contains("does not match its delivery and document")
        }));

        fs::write(
            acknowledgement,
            format!(
                "{{\"schemaVersion\":1,\"deliveryId\":\"{delivery_id}\",\"documentId\":\"{}\"}}\n",
                document.document_id
            ),
        )
        .unwrap();

        let synced = CompanionStatus::from_state(&document, &state, UNIX_EPOCH, None).unwrap();
        assert_eq!(synced.status, CompanionSyncStatus::Synced);
        assert!(!synced.supernote_pending);
        assert!(!synced.supernote_delivery_pending);
    }

    #[test]
    fn publishes_status_and_recovers_an_interrupted_previous_file() {
        let root = tempdir().unwrap();
        let document = document(root.path());
        let state = TransportState::empty();
        let destination = document
            .supernote_incoming_directory
            .join(COMPANION_STATUS_FILE);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        let previous = destination.with_extension("json.previous");
        fs::write(&previous, b"prior status").unwrap();

        publish_companion_status(&document, &state, UNIX_EPOCH, None).unwrap();
        let published: CompanionStatus =
            serde_json::from_slice(&fs::read(&destination).unwrap()).unwrap();
        assert_eq!(published.document_id, document.document_id);
        assert_eq!(published.status, CompanionSyncStatus::Synced);
        assert!(!previous.exists());
        assert!(!destination.with_extension("json.next").exists());
    }

    #[test]
    fn refuses_to_replace_a_symlink_status_destination() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let root = tempdir().unwrap();
            let document = document(root.path());
            fs::create_dir_all(&document.supernote_incoming_directory).unwrap();
            let outside = root.path().join("outside.json");
            fs::write(&outside, b"keep").unwrap();
            let destination = document
                .supernote_incoming_directory
                .join(COMPANION_STATUS_FILE);
            symlink(&outside, &destination).unwrap();
            let error =
                publish_companion_status(&document, &TransportState::empty(), UNIX_EPOCH, None)
                    .unwrap_err();
            assert!(error.contains("must not be a symlink"));
            assert_eq!(fs::read(&outside).unwrap(), b"keep");
        }
    }
}
