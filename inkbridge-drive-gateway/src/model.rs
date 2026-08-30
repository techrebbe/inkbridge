use inkbridge_broker::{DevicePayloadKind, DeviceSide, RevisionPair};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const DRIVE_GATEWAY_SCHEMA_VERSION: u32 = 1;
pub const DRIVE_GATEWAY_PRODUCER: &str = "inkbridge-drive-gateway";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DriveGatewayConfig {
    pub schema_version: u32,
    pub boox_folder_id: String,
    pub supernote_folder_id: String,
}

impl DriveGatewayConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != DRIVE_GATEWAY_SCHEMA_VERSION {
            return Err(format!(
                "unsupported Drive gateway schema version {}",
                self.schema_version
            ));
        }
        validate_drive_id("BOOX folder", &self.boox_folder_id)?;
        validate_drive_id("Supernote folder", &self.supernote_folder_id)?;
        if self.boox_folder_id == self.supernote_folder_id {
            return Err("BOOX and Supernote Drive folders must be distinct".to_owned());
        }
        Ok(())
    }

    pub fn folder_id(&self, side: DeviceSide) -> &str {
        match side {
            DeviceSide::Boox => &self.boox_folder_id,
            DeviceSide::Supernote => &self.supernote_folder_id,
        }
    }
}

fn validate_drive_id(label: &str, value: &str) -> Result<(), String> {
    if value.len() < 10
        || value.len() > 200
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(format!("{label} ID is invalid"));
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DriveFileRevision {
    pub file_id: String,
    pub name: String,
    pub version: u64,
    pub mime_type: String,
    pub parents: Vec<String>,
    pub size: u64,
    #[serde(default)]
    pub trashed: bool,
    #[serde(default)]
    pub app_properties: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DriveChange {
    pub file: DriveFileRevision,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DocumentBinding {
    pub document_id: String,
    pub original_pdf_sha256: String,
    #[serde(default)]
    pub boox_file_ids: BTreeSet<String>,
    #[serde(default)]
    pub supernote_file_ids: BTreeSet<String>,
}

impl DocumentBinding {
    pub fn side_for_file(&self, file_id: &str) -> Option<DeviceSide> {
        let boox = self.boox_file_ids.contains(file_id);
        let supernote = self.supernote_file_ids.contains(file_id);
        match (boox, supernote) {
            (true, false) => Some(DeviceSide::Boox),
            (false, true) => Some(DeviceSide::Supernote),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DriveGatewayCheckpoint {
    pub schema_version: u32,
    #[serde(default)]
    pub next_page_token: Option<String>,
    #[serde(default)]
    pub documents: BTreeMap<String, DocumentBinding>,
    #[serde(default)]
    pub processed_drive_events: BTreeSet<String>,
    #[serde(default)]
    pub accepted_file_content_sha256: BTreeMap<String, String>,
    #[serde(default)]
    pub file_observed_frontiers: BTreeMap<String, RevisionPair>,
    #[serde(default)]
    pub delivered_broker_outputs: BTreeMap<String, DeliveredDriveOutput>,
}

impl DriveGatewayCheckpoint {
    pub fn empty() -> Self {
        Self {
            schema_version: DRIVE_GATEWAY_SCHEMA_VERSION,
            ..Self::default()
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != DRIVE_GATEWAY_SCHEMA_VERSION {
            return Err(format!(
                "unsupported Drive gateway checkpoint schema version {}",
                self.schema_version
            ));
        }
        let mut all_file_ids = BTreeSet::new();
        for (document_id, binding) in &self.documents {
            if document_id != &binding.document_id {
                return Err(format!(
                    "document binding key {document_id} does not match {}",
                    binding.document_id
                ));
            }
            if !document_id.starts_with("inkbridge-doc-v1-")
                || binding.original_pdf_sha256.len() != 64
                || document_id != &format!("inkbridge-doc-v1-{}", binding.original_pdf_sha256)
            {
                return Err(format!("invalid stable document binding {document_id}"));
            }
            if !binding
                .boox_file_ids
                .is_disjoint(&binding.supernote_file_ids)
            {
                return Err(format!(
                    "document {document_id} binds one Drive file to both devices"
                ));
            }
            for file_id in binding
                .boox_file_ids
                .iter()
                .chain(&binding.supernote_file_ids)
            {
                if !all_file_ids.insert(file_id) {
                    return Err(format!(
                        "Drive file {file_id} is bound to more than one document"
                    ));
                }
            }
        }
        for (delivery_id, delivery) in &self.delivered_broker_outputs {
            if delivery_id != &delivery.delivery_id {
                return Err(format!(
                    "Drive delivery key {delivery_id} does not match {}",
                    delivery.delivery_id
                ));
            }
            if delivery.drive_file_version == 0 {
                return Err(format!("Drive delivery {delivery_id} has version zero"));
            }
            let binding = self.documents.get(&delivery.document_id).ok_or_else(|| {
                format!(
                    "Drive delivery {delivery_id} references unbound document {}",
                    delivery.document_id
                )
            })?;
            if binding.side_for_file(&delivery.drive_file_id) != Some(delivery.target) {
                return Err(format!(
                    "Drive delivery {delivery_id} file is not bound to its target side"
                ));
            }
        }
        for (file_id, content_sha256) in &self.accepted_file_content_sha256 {
            if !all_file_ids.contains(file_id) {
                return Err(format!(
                    "accepted content hash references unbound Drive file {file_id}"
                ));
            }
            if content_sha256.len() != 64
                || !content_sha256
                    .chars()
                    .all(|character| character.is_ascii_hexdigit())
            {
                return Err(format!(
                    "accepted content hash for Drive file {file_id} is invalid"
                ));
            }
        }
        for file_id in &all_file_ids {
            if !self.file_observed_frontiers.contains_key(file_id) {
                return Err(format!(
                    "bound Drive file {file_id} lacks an observed revision frontier"
                ));
            }
        }
        for file_id in self.file_observed_frontiers.keys() {
            if !all_file_ids.contains(file_id) {
                return Err(format!(
                    "observed revision frontier references unbound Drive file {file_id}"
                ));
            }
        }
        Ok(())
    }

    pub fn binding_for_file(&self, file_id: &str) -> Option<&DocumentBinding> {
        self.documents
            .values()
            .find(|binding| binding.side_for_file(file_id).is_some())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalFrontier {
    pub revisions: RevisionPair,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedDriveInput {
    pub drive_event_id: String,
    pub drive_file_id: String,
    pub gcs_object_path: String,
    pub content_sha256: String,
    pub metadata: BTreeMap<String, String>,
    pub document_id: String,
    pub source: DeviceSide,
    pub source_revision: u64,
    pub based_on: RevisionPair,
    pub payload_kind: DevicePayloadKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DriveInputDecision {
    Ignore { reason: String },
    Duplicate { drive_event_id: String },
    Unbound { file_id: String },
    Upload(PreparedDriveInput),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OriginalRegistrationApproval {
    pub drive_file_id: String,
    pub drive_file_version: u64,
    pub content_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedOriginalRegistration {
    pub drive_event_id: String,
    pub drive_file_id: String,
    pub source: DeviceSide,
    pub document_id: String,
    pub original_pdf_sha256: String,
    pub gcs_object_path: String,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceArtifactBindingApproval {
    pub drive_file_id: String,
    pub drive_file_version: u64,
    pub content_sha256: String,
    pub document_id: String,
    pub source: DeviceSide,
    pub based_on: RevisionPair,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedDeviceArtifactBinding {
    pub drive_file_id: String,
    pub drive_file_version: u64,
    pub content_sha256: String,
    pub document_id: String,
    pub source: DeviceSide,
    pub based_on: RevisionPair,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceArtifactBindingDecision {
    Ignore { reason: String },
    AlreadyBound { file_id: String },
    Bind(PreparedDeviceArtifactBinding),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistrationDecision {
    Ignore { reason: String },
    Duplicate { drive_event_id: String },
    Register(PreparedOriginalRegistration),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrokerDriveOutput {
    pub gcs_object_path: String,
    pub gcs_generation: u64,
    pub document_id: String,
    pub target: DeviceSide,
    pub event_id: String,
    pub source_revisions: RevisionPair,
    pub content_sha256: String,
    pub file_extension: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedDriveOutput {
    pub delivery_id: String,
    pub document_id: String,
    pub target: DeviceSide,
    pub content_sha256: String,
    pub source_revisions: RevisionPair,
    pub parent_folder_id: String,
    pub file_name: String,
    pub app_properties: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeliveredDriveOutput {
    pub delivery_id: String,
    pub drive_file_id: String,
    pub drive_file_version: u64,
    pub document_id: String,
    pub target: DeviceSide,
    pub content_sha256: String,
    pub source_revisions: RevisionPair,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DriveOutputDecision {
    Duplicate { delivery_id: String },
    Create(PreparedDriveOutput),
}
