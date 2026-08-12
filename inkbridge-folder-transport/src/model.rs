use inkbridge_broker::{DeviceSide, RevisionPair};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

pub const CONFIG_SCHEMA_VERSION: u32 = 1;
pub const TRANSPORT_STATE_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TransportConfig {
    pub schema_version: u32,
    pub bucket: String,
    #[serde(default = "default_gcloud")]
    pub gcloud_command: PathBuf,
    #[serde(default = "default_poll_seconds")]
    pub poll_seconds: u64,
    #[serde(default = "default_settle_seconds")]
    pub settle_seconds: u64,
    #[serde(default)]
    pub state_path: PathBuf,
    pub documents: Vec<DocumentFolders>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DocumentFolders {
    pub document_id: String,
    pub original_file_name: String,
    pub boox_pdf: PathBuf,
    pub supernote_export_directory: PathBuf,
    pub supernote_incoming_directory: PathBuf,
}

impl TransportConfig {
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        let mut config: Self = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid transport config: {error}"))?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        if config.state_path.as_os_str().is_empty() {
            config.state_path = base.join(".inkbridge-folder-state.json");
        } else if config.state_path.is_relative() {
            config.state_path = base.join(&config.state_path);
        }
        if config.gcloud_command.is_relative() && config.gcloud_command.components().count() > 1 {
            config.gcloud_command = base.join(&config.gcloud_command);
        }
        for document in &mut config.documents {
            resolve_path(base, &mut document.boox_pdf);
            resolve_path(base, &mut document.supernote_export_directory);
            resolve_path(base, &mut document.supernote_incoming_directory);
        }
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != CONFIG_SCHEMA_VERSION {
            return Err(format!(
                "unsupported transport config schema {}",
                self.schema_version
            ));
        }
        if self.bucket.trim().is_empty() || self.bucket.contains('/') {
            return Err("bucket must be a Cloud Storage bucket name".to_owned());
        }
        if self.poll_seconds == 0 {
            return Err("pollSeconds must be greater than zero".to_owned());
        }
        if self.documents.is_empty() {
            return Err("at least one document mapping is required".to_owned());
        }
        let mut ids = BTreeSet::new();
        let mut boox_paths = BTreeSet::new();
        let mut supernote_paths = BTreeSet::new();
        let state_path = (!self.state_path.as_os_str().is_empty())
            .then(|| normalized_path_key(&self.state_path))
            .transpose()?;
        for document in &self.documents {
            if !document.document_id.starts_with("inkbridge-doc-v1-")
                || document.document_id.contains(['/', '\\'])
            {
                return Err(format!(
                    "invalid stable documentId {}",
                    document.document_id
                ));
            }
            if !ids.insert(&document.document_id) {
                return Err(format!("duplicate documentId {}", document.document_id));
            }
            let boox_path = normalized_path_key(&document.boox_pdf)?;
            if !boox_paths.insert(boox_path.clone()) {
                return Err(format!(
                    "duplicate booxPdf mapping {}",
                    document.boox_pdf.display()
                ));
            }
            if state_path.as_deref() == Some(boox_path.as_str()) {
                return Err(format!(
                    "statePath {} collides with BOOX file {}",
                    self.state_path.display(),
                    document.boox_pdf.display()
                ));
            }
            if document.original_file_name.trim().is_empty() {
                return Err(format!(
                    "{} has an empty originalFileName",
                    document.document_id
                ));
            }
            if document.supernote_export_directory == document.supernote_incoming_directory {
                return Err(format!(
                    "{} must use separate Supernote outgoing and incoming directories",
                    document.document_id
                ));
            }
            for (direction, directory) in [
                ("outgoing", &document.supernote_export_directory),
                ("incoming", &document.supernote_incoming_directory),
            ] {
                let key = normalized_path_key(directory)?;
                if !supernote_paths.insert(key.clone()) {
                    return Err(format!(
                        "Supernote {direction} directory {} is shared by multiple mappings or directions",
                        directory.display()
                    ));
                }
                if state_path
                    .as_deref()
                    .is_some_and(|state| key_contains_path(&key, state))
                {
                    return Err(format!(
                        "statePath {} must be outside Supernote {direction} directory {}",
                        self.state_path.display(),
                        directory.display()
                    ));
                }
            }
        }
        Ok(())
    }
}

fn resolve_path(base: &Path, value: &mut PathBuf) {
    if value.is_relative() {
        *value = base.join(&*value);
    }
}

fn normalized_path_key(path: &Path) -> Result<String, String> {
    let resolved = resolve_existing_ancestor(path)?;
    let value = resolved.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        Ok(value.to_lowercase())
    } else {
        Ok(value)
    }
}

fn key_contains_path(directory: &str, candidate: &str) -> bool {
    candidate == directory
        || candidate
            .strip_prefix(directory)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn resolve_existing_ancestor(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("could not resolve current directory: {error}"))?
            .join(path)
    };
    let mut ancestor = absolute.as_path();
    let mut missing = Vec::<OsString>::new();
    loop {
        match ancestor.canonicalize() {
            Ok(mut resolved) => {
                for component in missing.into_iter().rev() {
                    resolved.push(component);
                }
                return Ok(lexical_normalize(&resolved));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let component = ancestor.components().next_back().ok_or_else(|| {
                    format!("could not find an existing ancestor for {}", path.display())
                })?;
                if matches!(component, Component::RootDir | Component::Prefix(_)) {
                    return Err(format!(
                        "could not find an existing ancestor for {}",
                        path.display()
                    ));
                }
                missing.push(component.as_os_str().to_os_string());
                ancestor = ancestor.parent().ok_or_else(|| {
                    format!("could not find an existing ancestor for {}", path.display())
                })?;
            }
            Err(error) => {
                return Err(format!(
                    "could not resolve mapped path {}: {error}",
                    path.display()
                ));
            }
        }
    }
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match normalized.components().next_back() {
                Some(Component::Normal(_)) => {
                    normalized.pop();
                }
                Some(Component::ParentDir) | None if !path.has_root() => normalized.push(".."),
                _ => {}
            },
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn default_gcloud() -> PathBuf {
    PathBuf::from(if cfg!(windows) {
        "gcloud.cmd"
    } else {
        "gcloud"
    })
}

fn default_poll_seconds() -> u64 {
    15
}

fn default_settle_seconds() -> u64 {
    10
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TransportState {
    pub schema_version: u32,
    #[serde(default)]
    pub documents: BTreeMap<String, DocumentTransportState>,
    #[serde(default)]
    pub observations: BTreeMap<String, FileObservation>,
}

impl TransportState {
    pub fn empty() -> Self {
        Self {
            schema_version: TRANSPORT_STATE_SCHEMA_VERSION,
            documents: BTreeMap::new(),
            observations: BTreeMap::new(),
        }
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        if !regular_file_exists(path)? {
            let previous = path.with_extension("json.previous");
            if regular_file_exists(&previous)? {
                return Self::load(&previous);
            }
            return Ok(Self::empty());
        }
        let bytes = std::fs::read(path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        let state: Self = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid transport state: {error}"))?;
        if state.schema_version != TRANSPORT_STATE_SCHEMA_VERSION {
            return Err(format!(
                "unsupported transport state schema {}",
                state.schema_version
            ));
        }
        Ok(state)
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
        let next = path.with_extension("json.next");
        let previous = path.with_extension("json.previous");
        let bytes = serde_json::to_vec_pretty(self).map_err(|error| error.to_string())?;
        std::fs::write(&next, [bytes.as_slice(), b"\n"].concat())
            .map_err(|error| format!("could not write {}: {error}", next.display()))?;
        let had_current = regular_file_exists(path)?;
        if had_current {
            remove_file_if_exists(&previous)?;
            std::fs::rename(path, &previous).map_err(|error| {
                format!(
                    "could not stage existing state {} as {}: {error}",
                    path.display(),
                    previous.display()
                )
            })?;
        }
        if let Err(error) = std::fs::rename(&next, path) {
            if had_current {
                let _ = std::fs::rename(&previous, path);
            }
            return Err(format!("could not publish {}: {error}", path.display()));
        }
        if had_current {
            remove_file_if_exists(&previous)
                .map_err(|error| format!("could not retire previous state: {error}"))?;
        }
        Ok(())
    }

    pub fn document_mut(&mut self, id: &str) -> &mut DocumentTransportState {
        self.documents.entry(id.to_owned()).or_default()
    }
}

fn regular_file_exists(path: &Path) -> Result<bool, String> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(format!("{} is not a regular file", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("could not inspect {}: {error}", path.display())),
    }
}

fn remove_file_if_exists(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("could not remove {}: {error}", path.display())),
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DocumentTransportState {
    #[serde(default)]
    pub revisions: RevisionPair,
    #[serde(default)]
    pub boox: SideTransportState,
    #[serde(default)]
    pub supernote: SideTransportState,
    #[serde(default)]
    pub delivered_generations: BTreeSet<String>,
    #[serde(default)]
    pub conflicts: BTreeSet<String>,
}

impl DocumentTransportState {
    pub fn side(&self, side: DeviceSide) -> &SideTransportState {
        match side {
            DeviceSide::Boox => &self.boox,
            DeviceSide::Supernote => &self.supernote,
        }
    }

    pub fn side_mut(&mut self, side: DeviceSide) -> &mut SideTransportState {
        match side {
            DeviceSide::Boox => &mut self.boox,
            DeviceSide::Supernote => &mut self.supernote,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SideTransportState {
    #[serde(default)]
    pub uploaded_local_hashes: BTreeMap<String, String>,
    #[serde(default)]
    pub accepted_local_hashes: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_content_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<PendingUpload>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PendingUpload {
    pub object_path: String,
    pub generation: u64,
    pub source_revision: u64,
    pub based_on: RevisionPair,
    pub local_path: String,
    pub local_content_sha256: String,
    pub payload_content_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileObservation {
    pub size: u64,
    pub modified_unix_millis: u64,
    pub first_seen_unix_millis: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_sha256: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudObject {
    pub path: String,
    pub generation: u64,
    pub size: u64,
    pub metadata: BTreeMap<String, String>,
}

impl CloudObject {
    pub fn generation_key(&self) -> String {
        format!("{}#{}", self.path, self.generation)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransportAction {
    Uploaded {
        side: DeviceSide,
        local_path: PathBuf,
        object_path: String,
        source_revision: u64,
        uploaded_bytes: u64,
    },
    Delivered {
        side: DeviceSide,
        object_path: String,
        local_path: PathBuf,
        generation: u64,
    },
    Deferred {
        side: DeviceSide,
        reason: String,
    },
    Conflict {
        object_path: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub actions: Vec<TransportAction>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn state_load_recovers_the_previous_checkpoint_after_interrupted_publish() {
        let directory = tempdir().unwrap();
        let state_path = directory.path().join("state.json");
        let previous = state_path.with_extension("json.previous");
        let mut expected = TransportState::empty();
        expected.documents.insert(
            "inkbridge-doc-v1-test".to_owned(),
            DocumentTransportState {
                revisions: RevisionPair {
                    boox: 2,
                    supernote: 3,
                },
                ..Default::default()
            },
        );
        std::fs::write(&previous, serde_json::to_vec(&expected).unwrap()).unwrap();

        assert_eq!(TransportState::load(&state_path).unwrap(), expected);
    }

    #[test]
    fn configuration_rejects_one_directory_for_both_supernote_directions() {
        let shared = PathBuf::from("shared");
        let config = TransportConfig {
            schema_version: CONFIG_SCHEMA_VERSION,
            bucket: "bucket".to_owned(),
            gcloud_command: default_gcloud(),
            poll_seconds: 1,
            settle_seconds: 0,
            state_path: PathBuf::new(),
            documents: vec![DocumentFolders {
                document_id: "inkbridge-doc-v1-test".to_owned(),
                original_file_name: "book.pdf".to_owned(),
                boox_pdf: PathBuf::from("boox.pdf"),
                supernote_export_directory: shared.clone(),
                supernote_incoming_directory: shared,
            }],
        };
        assert!(config.validate().unwrap_err().contains("separate"));
    }

    #[test]
    fn configuration_rejects_duplicate_boox_paths_across_documents() {
        let shared = PathBuf::from("boox/book.pdf");
        let config = TransportConfig {
            schema_version: CONFIG_SCHEMA_VERSION,
            bucket: "bucket".to_owned(),
            gcloud_command: default_gcloud(),
            poll_seconds: 1,
            settle_seconds: 0,
            state_path: PathBuf::new(),
            documents: vec![
                DocumentFolders {
                    document_id: "inkbridge-doc-v1-first".to_owned(),
                    original_file_name: "first.pdf".to_owned(),
                    boox_pdf: shared.clone(),
                    supernote_export_directory: PathBuf::from("first/outgoing"),
                    supernote_incoming_directory: PathBuf::from("first/incoming"),
                },
                DocumentFolders {
                    document_id: "inkbridge-doc-v1-second".to_owned(),
                    original_file_name: "second.pdf".to_owned(),
                    boox_pdf: shared,
                    supernote_export_directory: PathBuf::from("second/outgoing"),
                    supernote_incoming_directory: PathBuf::from("second/incoming"),
                },
            ],
        };

        assert!(config.validate().unwrap_err().contains("duplicate booxPdf"));
    }

    #[test]
    fn configuration_rejects_lexical_aliases_for_an_absent_boox_path() {
        let config = TransportConfig {
            schema_version: CONFIG_SCHEMA_VERSION,
            bucket: "bucket".to_owned(),
            gcloud_command: default_gcloud(),
            poll_seconds: 1,
            settle_seconds: 0,
            state_path: PathBuf::new(),
            documents: vec![
                DocumentFolders {
                    document_id: "inkbridge-doc-v1-first".to_owned(),
                    original_file_name: "first.pdf".to_owned(),
                    boox_pdf: PathBuf::from("missing/book.pdf"),
                    supernote_export_directory: PathBuf::from("first/outgoing"),
                    supernote_incoming_directory: PathBuf::from("first/incoming"),
                },
                DocumentFolders {
                    document_id: "inkbridge-doc-v1-second".to_owned(),
                    original_file_name: "second.pdf".to_owned(),
                    boox_pdf: PathBuf::from("missing/../missing/book.pdf"),
                    supernote_export_directory: PathBuf::from("second/outgoing"),
                    supernote_incoming_directory: PathBuf::from("second/incoming"),
                },
            ],
        };

        assert!(config.validate().unwrap_err().contains("duplicate booxPdf"));
    }

    #[test]
    fn configuration_rejects_shared_supernote_directories_across_documents() {
        let config = TransportConfig {
            schema_version: CONFIG_SCHEMA_VERSION,
            bucket: "bucket".to_owned(),
            gcloud_command: default_gcloud(),
            poll_seconds: 1,
            settle_seconds: 0,
            state_path: PathBuf::new(),
            documents: vec![
                DocumentFolders {
                    document_id: "inkbridge-doc-v1-first".to_owned(),
                    original_file_name: "book.pdf".to_owned(),
                    boox_pdf: PathBuf::from("first/book.pdf"),
                    supernote_export_directory: PathBuf::from("shared/outgoing"),
                    supernote_incoming_directory: PathBuf::from("first/incoming"),
                },
                DocumentFolders {
                    document_id: "inkbridge-doc-v1-second".to_owned(),
                    original_file_name: "book.pdf".to_owned(),
                    boox_pdf: PathBuf::from("second/book.pdf"),
                    supernote_export_directory: PathBuf::from("second/outgoing"),
                    supernote_incoming_directory: PathBuf::from("shared/outgoing"),
                },
            ],
        };

        assert!(config.validate().unwrap_err().contains("is shared"));
    }

    #[test]
    fn configuration_rejects_state_path_inside_a_mapped_device_path() {
        let config = TransportConfig {
            schema_version: CONFIG_SCHEMA_VERSION,
            bucket: "bucket".to_owned(),
            gcloud_command: default_gcloud(),
            poll_seconds: 1,
            settle_seconds: 0,
            state_path: PathBuf::from("supernote/outgoing/state.json"),
            documents: vec![DocumentFolders {
                document_id: "inkbridge-doc-v1-test".to_owned(),
                original_file_name: "book.pdf".to_owned(),
                boox_pdf: PathBuf::from("boox/book.pdf"),
                supernote_export_directory: PathBuf::from("supernote/outgoing"),
                supernote_incoming_directory: PathBuf::from("supernote/incoming"),
            }],
        };

        assert!(config.validate().unwrap_err().contains("must be outside"));
    }

    #[test]
    fn failed_state_publication_does_not_delete_the_recovery_checkpoint() {
        let directory = tempdir().unwrap();
        let state_path = directory.path().join("state.json");
        std::fs::create_dir(&state_path).unwrap();
        let previous = state_path.with_extension("json.previous");
        std::fs::write(&previous, b"durable recovery").unwrap();

        assert!(TransportState::empty().save(&state_path).is_err());
        assert_eq!(std::fs::read(previous).unwrap(), b"durable recovery");
    }

    #[cfg(unix)]
    #[test]
    fn absent_paths_resolve_through_the_nearest_symlink_ancestor() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let real = directory.path().join("real");
        let alias = directory.path().join("alias");
        std::fs::create_dir(&real).unwrap();
        symlink(&real, &alias).unwrap();

        assert_eq!(
            normalized_path_key(&real.join("missing/book.pdf")).unwrap(),
            normalized_path_key(&alias.join("missing/book.pdf")).unwrap()
        );
    }
}
