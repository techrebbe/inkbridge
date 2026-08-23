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
    /// Local mirror of `/storage/emulated/0/Documents/InkBridge` on the BOOX.
    /// When present, generated BOOX views use the versioned companion handoff
    /// instead of replacing each document's legacy `booxPdf` path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boox_handoff_root: Option<PathBuf>,
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

impl DocumentFolders {
    pub fn supernote_acknowledged_directory(&self) -> PathBuf {
        self.supernote_incoming_directory
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("acknowledged")
    }

    pub fn supernote_accepted_directory(&self) -> PathBuf {
        self.supernote_export_directory
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(".inkbridge-accepted")
    }
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
        if let Some(root) = &mut config.boox_handoff_root {
            resolve_path(base, root);
        }
        for document in &mut config.documents {
            resolve_path(base, &mut document.boox_pdf);
            resolve_path(base, &mut document.supernote_export_directory);
            resolve_path(base, &mut document.supernote_incoming_directory);
        }
        config.validate()?;
        config.validate_config_path(path)?;
        Ok(config)
    }

    fn validate_config_path(&self, path: &Path) -> Result<(), String> {
        let config_paths = path_key_variants(path)?;
        for reserved in state_reserved_paths(&self.state_path) {
            let reserved_paths = path_key_variants(&reserved)?;
            if path_sets_overlap(&config_paths, &reserved_paths) {
                return Err(format!(
                    "configuration file {} collides with statePath or checkpoint companion {}",
                    path.display(),
                    reserved.display()
                ));
            }
        }
        if let Some(root) = &self.boox_handoff_root {
            let root_paths = path_key_variants(root)?;
            if path_sets_overlap(&config_paths, &root_paths) {
                return Err(format!(
                    "configuration file {} must be outside booxHandoffRoot {}",
                    path.display(),
                    root.display()
                ));
            }
        }
        for document in &self.documents {
            let boox_paths = path_key_variants(&document.boox_pdf)?;
            if boox_paths.iter().any(|boox_path| {
                config_paths
                    .iter()
                    .any(|config_path| boox_mapping_overlaps_path(boox_path, config_path))
            }) {
                return Err(format!(
                    "configuration file {} must be outside mapped BOOX paths",
                    path.display()
                ));
            }
            for directory in [
                &document.supernote_export_directory,
                &document.supernote_incoming_directory,
                &document.supernote_acknowledged_directory(),
                &document.supernote_accepted_directory(),
            ] {
                let directory_paths = path_key_variants(directory)?;
                if path_sets_overlap(&config_paths, &directory_paths) {
                    return Err(format!(
                        "configuration file {} must be outside mapped Supernote directories",
                        path.display()
                    ));
                }
            }
        }
        Ok(())
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
        let mut boox_paths = BTreeSet::<String>::new();
        let mut supernote_paths = BTreeSet::<String>::new();
        let mut state_paths = Vec::<(PathBuf, String)>::new();
        if !self.state_path.as_os_str().is_empty() {
            for path in state_reserved_paths(&self.state_path) {
                for key in path_key_variants(&path)? {
                    state_paths.push((path.clone(), key));
                }
            }
        }
        if let Some(root) = &self.boox_handoff_root {
            reject_boox_handoff_leaf_symlink(root)?;
            let root_paths = path_key_variants(root)?;
            if let Some((reserved_path, _)) = state_paths.iter().find(|(_, state)| {
                root_paths
                    .iter()
                    .any(|root| key_contains_path(root, state) || key_contains_path(state, root))
            }) {
                return Err(format!(
                    "statePath or companion {} must be outside booxHandoffRoot {}",
                    reserved_path.display(),
                    root.display()
                ));
            }
            boox_paths.extend(root_paths);
        }
        for document in &self.documents {
            if !is_stable_document_id(&document.document_id) {
                return Err(format!(
                    "invalid stable documentId {}",
                    document.document_id
                ));
            }
            if !ids.insert(&document.document_id) {
                return Err(format!("duplicate documentId {}", document.document_id));
            }
            reject_boox_leaf_symlink(&document.boox_pdf)?;
            let document_boox_paths = path_key_variants(&document.boox_pdf)?;
            if document_boox_paths.iter().any(|boox_path| {
                boox_paths
                    .iter()
                    .any(|existing| boox_paths_conflict(existing, boox_path))
            }) {
                return Err(format!(
                    "booxPdf mapping {} collides with another BOOX file or reserved temporary path",
                    document.boox_pdf.display()
                ));
            }
            if document_boox_paths.iter().any(|boox_path| {
                supernote_paths
                    .iter()
                    .any(|directory| boox_mapping_overlaps_path(boox_path, directory))
            }) {
                return Err(format!(
                    "BOOX file {} overlaps a mapped Supernote directory",
                    document.boox_pdf.display()
                ));
            }
            if let Some((reserved_path, _)) = state_paths.iter().find(|(_, state)| {
                document_boox_paths
                    .iter()
                    .any(|boox_path| boox_mapping_overlaps_path(boox_path, state))
            }) {
                return Err(format!(
                    "statePath or companion {} collides with BOOX file {}",
                    reserved_path.display(),
                    document.boox_pdf.display()
                ));
            }
            boox_paths.extend(document_boox_paths);
            if document.original_file_name.trim().is_empty() {
                return Err(format!(
                    "{} has an empty originalFileName",
                    document.document_id
                ));
            }
            if self.boox_handoff_root.is_some()
                && (document.original_file_name.len() > 180
                    || Path::new(&document.original_file_name)
                        .file_name()
                        .and_then(|name| name.to_str())
                        != Some(document.original_file_name.as_str())
                    || matches!(document.original_file_name.as_str(), "." | "..")
                    || document.original_file_name.chars().any(|character| {
                        matches!(character, '/' | '\\' | '\0') || character.is_control()
                    }))
            {
                return Err(format!(
                    "{} originalFileName must be a safe BOOX handoff filename of at most 180 UTF-8 bytes",
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
                ("acknowledged", &document.supernote_acknowledged_directory()),
                ("accepted-cache", &document.supernote_accepted_directory()),
            ] {
                let directory_paths = path_key_variants(directory)?;
                if directory_paths.iter().any(|key| {
                    supernote_paths.iter().any(|existing| {
                        key_contains_path(existing, key) || key_contains_path(key, existing)
                    })
                }) {
                    return Err(format!(
                        "Supernote {direction} directory {} overlaps a directory used by another mapping or direction",
                        directory.display()
                    ));
                }
                if directory_paths.iter().any(|key| {
                    boox_paths
                        .iter()
                        .any(|boox| boox_mapping_overlaps_path(boox, key))
                }) {
                    return Err(format!(
                        "Supernote {direction} directory {} overlaps a mapped BOOX file",
                        directory.display()
                    ));
                }
                if let Some((reserved_path, _)) = state_paths.iter().find(|(_, state)| {
                    directory_paths
                        .iter()
                        .any(|key| key_contains_path(key, state) || key_contains_path(state, key))
                }) {
                    return Err(format!(
                        "statePath or companion {} must be outside Supernote {direction} directory {}",
                        reserved_path.display(),
                        directory.display()
                    ));
                }
                supernote_paths.extend(directory_paths);
            }
        }
        Ok(())
    }
}

fn is_stable_document_id(value: &str) -> bool {
    value.strip_prefix("inkbridge-doc-v1-").is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
}

fn state_reserved_paths(path: &Path) -> [PathBuf; 4] {
    [
        path.to_path_buf(),
        path.with_extension("json.next"),
        path.with_extension("json.previous"),
        path.with_extension("json.lock"),
    ]
}

fn resolve_path(base: &Path, value: &mut PathBuf) {
    if value.is_relative() {
        *value = base.join(&*value);
    }
}

fn normalized_path_key(path: &Path) -> Result<String, String> {
    let resolved = resolve_existing_ancestor(path)?;
    Ok(platform_path_key(&resolved))
}

fn lexical_path_key(path: &Path) -> Result<String, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("could not resolve current directory: {error}"))?
            .join(path)
    };
    Ok(platform_path_key(&lexical_normalize(&absolute)))
}

fn path_key_variants(path: &Path) -> Result<BTreeSet<String>, String> {
    Ok(BTreeSet::from([
        lexical_path_key(path)?,
        normalized_path_key(path)?,
    ]))
}

fn platform_path_key(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value
    }
}

fn key_contains_path(directory: &str, candidate: &str) -> bool {
    Path::new(candidate).starts_with(Path::new(directory))
}

fn path_sets_overlap(left: &BTreeSet<String>, right: &BTreeSet<String>) -> bool {
    left.iter().any(|left| {
        right
            .iter()
            .any(|right| key_contains_path(left, right) || key_contains_path(right, left))
    })
}

fn reject_boox_leaf_symlink(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "booxPdf {} must not be a leaf symlink; use its target path or a symlinked parent directory",
            path.display()
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "could not inspect booxPdf entry {}: {error}",
            path.display()
        )),
    }
}

fn reject_boox_handoff_leaf_symlink(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "booxHandoffRoot {} must not be a leaf symlink; use its target path or a symlinked parent directory",
            path.display()
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "could not inspect booxHandoffRoot entry {}: {error}",
            path.display()
        )),
    }
}

fn boox_paths_conflict(left: &str, right: &str) -> bool {
    left == right
        || is_at_or_below_boox_temporary(left, right)
        || is_at_or_below_boox_temporary(right, left)
}

fn boox_mapping_overlaps_path(boox: &str, other: &str) -> bool {
    key_contains_path(other, boox)
        || key_contains_path(boox, other)
        || boox_static_temporary_keys(boox).iter().any(|temporary| {
            key_contains_path(other, temporary) || key_contains_path(temporary, other)
        })
        || is_at_or_below_boox_temporary(other, boox)
}

fn boox_static_temporary_keys(boox: &str) -> [String; 2] {
    [
        sibling_temporary_key(boox, "compact-upload"),
        sibling_temporary_key(boox, "previous"),
    ]
}

fn sibling_temporary_key(path: &str, suffix: &str) -> String {
    let path = Path::new(path);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("inkbridge");
    path.with_file_name(format!(".{name}.{suffix}.part"))
        .to_string_lossy()
        .replace('\\', "/")
}

fn is_boox_temporary_for(candidate: &str, boox: &str) -> bool {
    let candidate = Path::new(candidate);
    let boox = Path::new(boox);
    if candidate.parent() != boox.parent() {
        return false;
    }
    let Some(candidate_name) = candidate.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(boox_name) = boox.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if candidate_name == format!(".{boox_name}.compact-upload.part")
        || candidate_name == format!(".{boox_name}.previous.part")
    {
        return true;
    }
    candidate_name
        .strip_prefix(&format!(".{boox_name}.g"))
        .and_then(|suffix| suffix.strip_suffix(".part"))
        .is_some_and(|generation| {
            !generation.is_empty() && generation.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn is_at_or_below_boox_temporary(candidate: &str, boox: &str) -> bool {
    Path::new(candidate)
        .ancestors()
        .any(|ancestor| is_boox_temporary_for(&ancestor.to_string_lossy().replace('\\', "/"), boox))
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
                match std::fs::symlink_metadata(ancestor) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        return Err(format!(
                            "mapped path {} contains dangling symlink {}",
                            path.display(),
                            ancestor.display()
                        ));
                    }
                    Ok(_) => {}
                    Err(metadata_error)
                        if metadata_error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(metadata_error) => {
                        return Err(format!(
                            "could not inspect mapped path component {}: {metadata_error}",
                            ancestor.display()
                        ));
                    }
                }
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
        remove_file_if_exists(&previous)
            .map_err(|error| format!("could not retire previous state: {error}"))?;
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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub recovered_source_identities: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub accepted_source_revisions: BTreeMap<String, u64>,
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

    fn test_document_id(nibble: char) -> String {
        format!("inkbridge-doc-v1-{}", nibble.to_string().repeat(64))
    }

    #[test]
    fn state_load_recovers_the_previous_checkpoint_after_interrupted_publish() {
        let directory = tempdir().unwrap();
        let state_path = directory.path().join("state.json");
        let previous = state_path.with_extension("json.previous");
        let mut expected = TransportState::empty();
        expected.documents.insert(
            test_document_id('a'),
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
    fn configuration_rejects_malformed_stable_document_ids() {
        for document_id in [
            "inkbridge-doc-v1-deadbeef".to_owned(),
            format!("inkbridge-doc-v1-{}", "A".repeat(64)),
            format!("inkbridge-doc-v1-{}g", "a".repeat(63)),
        ] {
            let config = TransportConfig {
                schema_version: CONFIG_SCHEMA_VERSION,
                bucket: "bucket".to_owned(),
                gcloud_command: default_gcloud(),
                poll_seconds: 1,
                settle_seconds: 0,
                state_path: PathBuf::new(),
                boox_handoff_root: None,
                documents: vec![DocumentFolders {
                    document_id,
                    original_file_name: "book.pdf".to_owned(),
                    boox_pdf: PathBuf::from("boox.pdf"),
                    supernote_export_directory: PathBuf::from("supernote/outgoing"),
                    supernote_incoming_directory: PathBuf::from("supernote/incoming"),
                }],
            };

            assert!(config
                .validate()
                .unwrap_err()
                .contains("invalid stable documentId"));
        }
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
            boox_handoff_root: None,
            documents: vec![DocumentFolders {
                document_id: test_document_id('a'),
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
            boox_handoff_root: None,
            documents: vec![
                DocumentFolders {
                    document_id: test_document_id('a'),
                    original_file_name: "first.pdf".to_owned(),
                    boox_pdf: shared.clone(),
                    supernote_export_directory: PathBuf::from("first/outgoing"),
                    supernote_incoming_directory: PathBuf::from("first/incoming"),
                },
                DocumentFolders {
                    document_id: test_document_id('b'),
                    original_file_name: "second.pdf".to_owned(),
                    boox_pdf: shared,
                    supernote_export_directory: PathBuf::from("second/outgoing"),
                    supernote_incoming_directory: PathBuf::from("second/incoming"),
                },
            ],
        };

        assert!(config
            .validate()
            .unwrap_err()
            .contains("reserved temporary"));
    }

    #[test]
    fn configuration_rejects_boox_temporary_path_collisions() {
        for second_path in [
            "boox/.book.pdf.compact-upload.part",
            "boox/.book.pdf.previous.part",
            "boox/.book.pdf.g42.part",
            "boox/.book.pdf.g42.part/nested.pdf",
        ] {
            let config = TransportConfig {
                schema_version: CONFIG_SCHEMA_VERSION,
                bucket: "bucket".to_owned(),
                gcloud_command: default_gcloud(),
                poll_seconds: 1,
                settle_seconds: 0,
                state_path: PathBuf::new(),
                boox_handoff_root: None,
                documents: vec![
                    DocumentFolders {
                        document_id: test_document_id('a'),
                        original_file_name: "first.pdf".to_owned(),
                        boox_pdf: PathBuf::from("boox/book.pdf"),
                        supernote_export_directory: PathBuf::from("first/outgoing"),
                        supernote_incoming_directory: PathBuf::from("first/incoming"),
                    },
                    DocumentFolders {
                        document_id: test_document_id('b'),
                        original_file_name: "second.pdf".to_owned(),
                        boox_pdf: PathBuf::from(second_path),
                        supernote_export_directory: PathBuf::from("second/outgoing"),
                        supernote_incoming_directory: PathBuf::from("second/incoming"),
                    },
                ],
            };

            assert!(config
                .validate()
                .unwrap_err()
                .contains("reserved temporary"));
        }
    }

    #[test]
    fn configuration_rejects_state_and_supernote_temporary_path_collisions() {
        let state_collision = TransportConfig {
            schema_version: CONFIG_SCHEMA_VERSION,
            bucket: "bucket".to_owned(),
            gcloud_command: default_gcloud(),
            poll_seconds: 1,
            settle_seconds: 0,
            state_path: PathBuf::from("boox/.book.pdf.g7.part"),
            boox_handoff_root: None,
            documents: vec![DocumentFolders {
                document_id: test_document_id('a'),
                original_file_name: "book.pdf".to_owned(),
                boox_pdf: PathBuf::from("boox/book.pdf"),
                supernote_export_directory: PathBuf::from("supernote/outgoing"),
                supernote_incoming_directory: PathBuf::from("supernote/incoming"),
            }],
        };
        assert!(state_collision.validate().unwrap_err().contains("collides"));

        let supernote_collision = TransportConfig {
            state_path: PathBuf::new(),
            boox_handoff_root: None,
            documents: vec![DocumentFolders {
                document_id: test_document_id('a'),
                original_file_name: "book.pdf".to_owned(),
                boox_pdf: PathBuf::from("boox/book.pdf"),
                supernote_export_directory: PathBuf::from("boox/.book.pdf.compact-upload.part"),
                supernote_incoming_directory: PathBuf::from("supernote/incoming"),
            }],
            ..state_collision.clone()
        };
        assert!(supernote_collision
            .validate()
            .unwrap_err()
            .contains("overlaps"));

        let generation_descendant_collision = TransportConfig {
            state_path: PathBuf::new(),
            boox_handoff_root: None,
            documents: vec![DocumentFolders {
                document_id: test_document_id('a'),
                original_file_name: "book.pdf".to_owned(),
                boox_pdf: PathBuf::from("boox/book.pdf"),
                supernote_export_directory: PathBuf::from("boox/.book.pdf.g7.part/incoming"),
                supernote_incoming_directory: PathBuf::from("supernote/incoming"),
            }],
            ..state_collision
        };
        assert!(generation_descendant_collision
            .validate()
            .unwrap_err()
            .contains("overlaps"));
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
            boox_handoff_root: None,
            documents: vec![
                DocumentFolders {
                    document_id: test_document_id('a'),
                    original_file_name: "first.pdf".to_owned(),
                    boox_pdf: PathBuf::from("missing/book.pdf"),
                    supernote_export_directory: PathBuf::from("first/outgoing"),
                    supernote_incoming_directory: PathBuf::from("first/incoming"),
                },
                DocumentFolders {
                    document_id: test_document_id('b'),
                    original_file_name: "second.pdf".to_owned(),
                    boox_pdf: PathBuf::from("missing/../missing/book.pdf"),
                    supernote_export_directory: PathBuf::from("second/outgoing"),
                    supernote_incoming_directory: PathBuf::from("second/incoming"),
                },
            ],
        };

        assert!(config
            .validate()
            .unwrap_err()
            .contains("reserved temporary"));
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
            boox_handoff_root: None,
            documents: vec![
                DocumentFolders {
                    document_id: test_document_id('a'),
                    original_file_name: "book.pdf".to_owned(),
                    boox_pdf: PathBuf::from("first/book.pdf"),
                    supernote_export_directory: PathBuf::from("shared/outgoing"),
                    supernote_incoming_directory: PathBuf::from("first/incoming"),
                },
                DocumentFolders {
                    document_id: test_document_id('b'),
                    original_file_name: "book.pdf".to_owned(),
                    boox_pdf: PathBuf::from("second/book.pdf"),
                    supernote_export_directory: PathBuf::from("second/outgoing"),
                    supernote_incoming_directory: PathBuf::from("shared/outgoing"),
                },
            ],
        };

        assert!(config
            .validate()
            .unwrap_err()
            .contains("overlaps a directory"));
    }

    #[test]
    fn configuration_reserves_accepted_cache_against_other_mappings() {
        let config = TransportConfig {
            schema_version: CONFIG_SCHEMA_VERSION,
            bucket: "bucket".to_owned(),
            gcloud_command: default_gcloud(),
            poll_seconds: 1,
            settle_seconds: 0,
            state_path: PathBuf::new(),
            boox_handoff_root: None,
            documents: vec![
                DocumentFolders {
                    document_id: test_document_id('a'),
                    original_file_name: "first.pdf".to_owned(),
                    boox_pdf: PathBuf::from("first/book.pdf"),
                    supernote_export_directory: PathBuf::from("shared/outgoing"),
                    supernote_incoming_directory: PathBuf::from("shared/incoming"),
                },
                DocumentFolders {
                    document_id: test_document_id('b'),
                    original_file_name: "second.pdf".to_owned(),
                    boox_pdf: PathBuf::from("second/book.pdf"),
                    supernote_export_directory: PathBuf::from("shared/.inkbridge-accepted"),
                    supernote_incoming_directory: PathBuf::from("second/incoming"),
                },
            ],
        };

        assert!(config
            .validate()
            .unwrap_err()
            .contains("overlaps a directory"));
    }

    #[test]
    fn configuration_reserves_accepted_cache_against_boox_and_state_paths() {
        let document = DocumentFolders {
            document_id: test_document_id('a'),
            original_file_name: "book.pdf".to_owned(),
            boox_pdf: PathBuf::from("supernote/.inkbridge-accepted/book.pdf"),
            supernote_export_directory: PathBuf::from("supernote/outgoing"),
            supernote_incoming_directory: PathBuf::from("supernote/incoming"),
        };
        let boox_collision = TransportConfig {
            schema_version: CONFIG_SCHEMA_VERSION,
            bucket: "bucket".to_owned(),
            gcloud_command: default_gcloud(),
            poll_seconds: 1,
            settle_seconds: 0,
            state_path: PathBuf::new(),
            boox_handoff_root: None,
            documents: vec![document.clone()],
        };
        assert!(boox_collision.validate().unwrap_err().contains("BOOX"));

        let state_collision = TransportConfig {
            state_path: PathBuf::from("supernote/.inkbridge-accepted/state.json"),
            boox_handoff_root: None,
            documents: vec![DocumentFolders {
                boox_pdf: PathBuf::from("boox/book.pdf"),
                ..document
            }],
            ..boox_collision
        };
        assert!(state_collision
            .validate()
            .unwrap_err()
            .contains("must be outside"));
    }

    #[test]
    fn configuration_rejects_nested_supernote_directories() {
        let config = TransportConfig {
            schema_version: CONFIG_SCHEMA_VERSION,
            bucket: "bucket".to_owned(),
            gcloud_command: default_gcloud(),
            poll_seconds: 1,
            settle_seconds: 0,
            state_path: PathBuf::new(),
            boox_handoff_root: None,
            documents: vec![
                DocumentFolders {
                    document_id: test_document_id('a'),
                    original_file_name: "first.pdf".to_owned(),
                    boox_pdf: PathBuf::from("first/book.pdf"),
                    supernote_export_directory: PathBuf::from("supernote/root"),
                    supernote_incoming_directory: PathBuf::from("first/incoming"),
                },
                DocumentFolders {
                    document_id: test_document_id('b'),
                    original_file_name: "second.pdf".to_owned(),
                    boox_pdf: PathBuf::from("second/book.pdf"),
                    supernote_export_directory: PathBuf::from("second/outgoing"),
                    supernote_incoming_directory: PathBuf::from(
                        "supernote/root/.event.operations.json.g7.part/incoming",
                    ),
                },
            ],
        };

        assert!(config
            .validate()
            .unwrap_err()
            .contains("overlaps a directory"));
    }

    #[test]
    fn configuration_rejects_overlong_file_name_when_boox_handoff_is_enabled() {
        let mut config = TransportConfig {
            schema_version: CONFIG_SCHEMA_VERSION,
            bucket: "bucket".to_owned(),
            gcloud_command: default_gcloud(),
            poll_seconds: 1,
            settle_seconds: 0,
            state_path: PathBuf::from("state.json"),
            boox_handoff_root: Some(PathBuf::from("boox-handoff")),
            documents: vec![DocumentFolders {
                document_id: test_document_id('a'),
                original_file_name: format!("{}.pdf", "a".repeat(177)),
                boox_pdf: PathBuf::from("legacy/book.pdf"),
                supernote_export_directory: PathBuf::from("supernote/outgoing"),
                supernote_incoming_directory: PathBuf::from("supernote/incoming"),
            }],
        };

        assert!(config
            .validate()
            .unwrap_err()
            .contains("at most 180 UTF-8 bytes"));

        config.boox_handoff_root = None;
        config
            .validate()
            .expect("the legacy path does not use the handoff filename protocol");
    }

    #[test]
    fn configuration_load_resolves_relative_boox_handoff_root() {
        let directory = tempdir().unwrap();
        let config_path = directory.path().join("inkbridge-folder-transport.json");
        let config = TransportConfig {
            schema_version: CONFIG_SCHEMA_VERSION,
            bucket: "bucket".to_owned(),
            gcloud_command: default_gcloud(),
            poll_seconds: 1,
            settle_seconds: 0,
            state_path: PathBuf::from("state.json"),
            boox_handoff_root: Some(PathBuf::from("boox-handoff")),
            documents: vec![DocumentFolders {
                document_id: test_document_id('a'),
                original_file_name: "book.pdf".to_owned(),
                boox_pdf: PathBuf::from("legacy/book.pdf"),
                supernote_export_directory: PathBuf::from("supernote/outgoing"),
                supernote_incoming_directory: PathBuf::from("supernote/incoming"),
            }],
        };
        std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();

        let loaded = TransportConfig::load(&config_path).unwrap();
        assert_eq!(
            loaded.boox_handoff_root,
            Some(directory.path().join("boox-handoff"))
        );
    }

    #[test]
    fn configuration_rejects_supernote_directory_inside_boox_handoff_root() {
        let config = TransportConfig {
            schema_version: CONFIG_SCHEMA_VERSION,
            bucket: "bucket".to_owned(),
            gcloud_command: default_gcloud(),
            poll_seconds: 1,
            settle_seconds: 0,
            state_path: PathBuf::from("state.json"),
            boox_handoff_root: Some(PathBuf::from("boox-handoff")),
            documents: vec![DocumentFolders {
                document_id: test_document_id('a'),
                original_file_name: "book.pdf".to_owned(),
                boox_pdf: PathBuf::from("legacy/book.pdf"),
                supernote_export_directory: PathBuf::from("boox-handoff/supernote/outgoing"),
                supernote_incoming_directory: PathBuf::from("supernote/incoming"),
            }],
        };

        assert!(config.validate().unwrap_err().contains("overlaps"));
    }
    #[test]
    fn configuration_load_rejects_its_file_inside_supernote_outgoing() {
        let directory = tempdir().unwrap();
        let outgoing = directory.path().join("supernote/outgoing");
        std::fs::create_dir_all(&outgoing).unwrap();
        let config_path = outgoing.join("inkbridge-folder-transport.json");
        let config = TransportConfig {
            schema_version: CONFIG_SCHEMA_VERSION,
            bucket: "bucket".to_owned(),
            gcloud_command: default_gcloud(),
            poll_seconds: 1,
            settle_seconds: 0,
            state_path: directory.path().join("state.json"),
            boox_handoff_root: None,
            documents: vec![DocumentFolders {
                document_id: test_document_id('a'),
                original_file_name: "book.pdf".to_owned(),
                boox_pdf: directory.path().join("boox/book.pdf"),
                supernote_export_directory: outgoing,
                supernote_incoming_directory: directory.path().join("supernote/incoming"),
            }],
        };
        std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();

        assert!(TransportConfig::load(&config_path)
            .unwrap_err()
            .contains("configuration file"));
    }

    #[test]
    fn configuration_load_rejects_its_file_inside_accepted_cache() {
        let directory = tempdir().unwrap();
        let outgoing = directory.path().join("supernote/outgoing");
        let accepted = directory.path().join("supernote/.inkbridge-accepted");
        std::fs::create_dir_all(&accepted).unwrap();
        let config_path = accepted.join("inkbridge-folder-transport.json");
        let config = TransportConfig {
            schema_version: CONFIG_SCHEMA_VERSION,
            bucket: "bucket".to_owned(),
            gcloud_command: default_gcloud(),
            poll_seconds: 1,
            settle_seconds: 0,
            state_path: directory.path().join("state.json"),
            boox_handoff_root: None,
            documents: vec![DocumentFolders {
                document_id: test_document_id('a'),
                original_file_name: "book.pdf".to_owned(),
                boox_pdf: directory.path().join("boox/book.pdf"),
                supernote_export_directory: outgoing,
                supernote_incoming_directory: directory.path().join("supernote/incoming"),
            }],
        };
        std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();

        assert!(TransportConfig::load(&config_path)
            .unwrap_err()
            .contains("configuration file"));
    }

    #[test]
    fn configuration_load_rejects_checkpoint_companion_as_config_file() {
        let directory = tempdir().unwrap();
        let state_path = directory.path().join("state.json");
        let config_path = state_path.with_extension("json.next");
        let config = TransportConfig {
            schema_version: CONFIG_SCHEMA_VERSION,
            bucket: "bucket".to_owned(),
            gcloud_command: default_gcloud(),
            poll_seconds: 1,
            settle_seconds: 0,
            state_path,
            boox_handoff_root: None,
            documents: vec![DocumentFolders {
                document_id: test_document_id('a'),
                original_file_name: "book.pdf".to_owned(),
                boox_pdf: directory.path().join("boox/book.pdf"),
                supernote_export_directory: directory.path().join("supernote/outgoing"),
                supernote_incoming_directory: directory.path().join("supernote/incoming"),
            }],
        };
        std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();

        assert!(TransportConfig::load(&config_path)
            .unwrap_err()
            .contains("checkpoint companion"));
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
            boox_handoff_root: None,
            documents: vec![DocumentFolders {
                document_id: test_document_id('a'),
                original_file_name: "book.pdf".to_owned(),
                boox_pdf: PathBuf::from("boox/book.pdf"),
                supernote_export_directory: PathBuf::from("supernote/outgoing"),
                supernote_incoming_directory: PathBuf::from("supernote/incoming"),
            }],
        };

        assert!(config.validate().unwrap_err().contains("must be outside"));
    }

    #[test]
    fn configuration_rejects_checkpoint_companion_collisions() {
        for companion in ["state.json.next", "state.json.previous", "state.json.lock"] {
            let config = TransportConfig {
                schema_version: CONFIG_SCHEMA_VERSION,
                bucket: "bucket".to_owned(),
                gcloud_command: default_gcloud(),
                poll_seconds: 1,
                settle_seconds: 0,
                state_path: PathBuf::from("state.json"),
                boox_handoff_root: None,
                documents: vec![DocumentFolders {
                    document_id: test_document_id('a'),
                    original_file_name: "book.pdf".to_owned(),
                    boox_pdf: PathBuf::from(companion),
                    supernote_export_directory: PathBuf::from("supernote/outgoing"),
                    supernote_incoming_directory: PathBuf::from("supernote/incoming"),
                }],
            };

            assert!(config.validate().unwrap_err().contains("collides"));
        }
    }

    #[test]
    fn configuration_rejects_state_path_above_a_mapped_directory() {
        let config = TransportConfig {
            schema_version: CONFIG_SCHEMA_VERSION,
            bucket: "bucket".to_owned(),
            gcloud_command: default_gcloud(),
            poll_seconds: 1,
            settle_seconds: 0,
            state_path: PathBuf::from("supernote"),
            boox_handoff_root: None,
            documents: vec![DocumentFolders {
                document_id: test_document_id('a'),
                original_file_name: "book.pdf".to_owned(),
                boox_pdf: PathBuf::from("boox/book.pdf"),
                supernote_export_directory: PathBuf::from("supernote/outgoing"),
                supernote_incoming_directory: PathBuf::from("supernote/incoming"),
            }],
        };

        assert!(config.validate().unwrap_err().contains("must be outside"));
    }

    #[test]
    fn filesystem_root_contains_descendant_paths() {
        let current = std::env::current_dir().unwrap();
        let root = current.ancestors().last().unwrap();
        let root_key = normalized_path_key(root).unwrap();
        let state_key = normalized_path_key(&root.join("inkbridge-state.json")).unwrap();

        assert!(key_contains_path(&root_key, &state_key));
    }

    #[test]
    fn configuration_rejects_overlap_between_boox_and_supernote_paths() {
        let config = TransportConfig {
            schema_version: CONFIG_SCHEMA_VERSION,
            bucket: "bucket".to_owned(),
            gcloud_command: default_gcloud(),
            poll_seconds: 1,
            settle_seconds: 0,
            state_path: PathBuf::from("state.json"),
            boox_handoff_root: None,
            documents: vec![DocumentFolders {
                document_id: test_document_id('a'),
                original_file_name: "book.pdf".to_owned(),
                boox_pdf: PathBuf::from("shared/book.pdf"),
                supernote_export_directory: PathBuf::from("shared"),
                supernote_incoming_directory: PathBuf::from("supernote/incoming"),
            }],
        };

        assert!(config.validate().unwrap_err().contains("overlaps"));
    }

    #[test]
    fn configuration_rejects_cross_document_boox_supernote_overlap() {
        let config = TransportConfig {
            schema_version: CONFIG_SCHEMA_VERSION,
            bucket: "bucket".to_owned(),
            gcloud_command: default_gcloud(),
            poll_seconds: 1,
            settle_seconds: 0,
            state_path: PathBuf::from("state.json"),
            boox_handoff_root: None,
            documents: vec![
                DocumentFolders {
                    document_id: test_document_id('a'),
                    original_file_name: "first.pdf".to_owned(),
                    boox_pdf: PathBuf::from("first/book.pdf"),
                    supernote_export_directory: PathBuf::from("shared"),
                    supernote_incoming_directory: PathBuf::from("first/incoming"),
                },
                DocumentFolders {
                    document_id: test_document_id('b'),
                    original_file_name: "second.pdf".to_owned(),
                    boox_pdf: PathBuf::from("shared/second.pdf"),
                    supernote_export_directory: PathBuf::from("second/outgoing"),
                    supernote_incoming_directory: PathBuf::from("second/incoming"),
                },
            ],
        };

        assert!(config.validate().unwrap_err().contains("overlaps"));
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

    #[cfg(unix)]
    #[test]
    fn configuration_load_rejects_symlink_entry_inside_supernote_outgoing() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let outgoing = directory.path().join("supernote/outgoing");
        std::fs::create_dir_all(&outgoing).unwrap();
        let target = directory.path().join("transport-config.json");
        let entry = outgoing.join("transport-config-link.json");
        let config = TransportConfig {
            schema_version: CONFIG_SCHEMA_VERSION,
            bucket: "bucket".to_owned(),
            gcloud_command: default_gcloud(),
            poll_seconds: 1,
            settle_seconds: 0,
            state_path: directory.path().join("state.json"),
            boox_handoff_root: None,
            documents: vec![DocumentFolders {
                document_id: test_document_id('a'),
                original_file_name: "book.pdf".to_owned(),
                boox_pdf: directory.path().join("boox/book.pdf"),
                supernote_export_directory: outgoing,
                supernote_incoming_directory: directory.path().join("supernote/incoming"),
            }],
        };
        std::fs::write(&target, serde_json::to_vec(&config).unwrap()).unwrap();
        symlink(&target, &entry).unwrap();

        assert!(TransportConfig::load(&entry)
            .unwrap_err()
            .contains("mapped Supernote"));
    }

    #[cfg(unix)]
    #[test]
    fn configuration_rejects_a_symlinked_boox_leaf() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let entry_directory = directory.path().join("boox-entry");
        let target_directory = directory.path().join("boox-target");
        std::fs::create_dir_all(&entry_directory).unwrap();
        std::fs::create_dir_all(&target_directory).unwrap();
        let target = target_directory.join("book.pdf");
        let entry = entry_directory.join("book.pdf");
        std::fs::write(&target, b"pdf").unwrap();
        symlink(&target, &entry).unwrap();
        let config = TransportConfig {
            schema_version: CONFIG_SCHEMA_VERSION,
            bucket: "bucket".to_owned(),
            gcloud_command: default_gcloud(),
            poll_seconds: 1,
            settle_seconds: 0,
            state_path: PathBuf::new(),
            boox_handoff_root: None,
            documents: vec![DocumentFolders {
                document_id: test_document_id('a'),
                original_file_name: "first.pdf".to_owned(),
                boox_pdf: entry,
                supernote_export_directory: directory.path().join("first/outgoing"),
                supernote_incoming_directory: directory.path().join("first/incoming"),
            }],
        };

        assert!(config
            .validate()
            .unwrap_err()
            .contains("must not be a leaf symlink"));
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlink_ancestors_are_rejected() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let target = directory.path().join("missing-target");
        let alias = directory.path().join("alias");
        symlink(&target, &alias).unwrap();

        assert!(normalized_path_key(&alias.join("book.pdf"))
            .unwrap_err()
            .contains("dangling symlink"));
    }
}
