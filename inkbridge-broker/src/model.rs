use inkbridge_convert::StrokeSnapshot;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const BROKER_PRODUCER: &str = "inkbridge-broker";
pub const STATE_SCHEMA_VERSION: u32 = 1;
pub const EVENT_SCHEMA_VERSION: u32 = 1;

pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn stable_document_id(original_pdf: &[u8]) -> String {
    format!("inkbridge-doc-v1-{}", sha256_hex(original_pdf))
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeviceSide {
    Boox,
    Supernote,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DevicePayloadKind {
    #[default]
    DeviceView,
    BooxOperationManifest,
}

impl DeviceSide {
    pub fn other(self) -> Self {
        match self {
            Self::Boox => Self::Supernote,
            Self::Supernote => Self::Boox,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RevisionPair {
    pub boox: u64,
    pub supernote: u64,
}

impl RevisionPair {
    pub fn get(self, side: DeviceSide) -> u64 {
        match side {
            DeviceSide::Boox => self.boox,
            DeviceSide::Supernote => self.supernote,
        }
    }

    pub fn set(&mut self, side: DeviceSide, revision: u64) {
        match side {
            DeviceSide::Boox => self.boox = revision,
            DeviceSide::Supernote => self.supernote = revision,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BrokerOutputMarker {
    pub producer: String,
    pub event_id: String,
    pub document_id: String,
    pub source_revisions: RevisionPair,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StorageEvent {
    pub schema_version: u32,
    pub event_id: String,
    pub document_id: String,
    pub source: DeviceSide,
    pub object_path: String,
    pub source_generation: u64,
    pub source_revision: u64,
    pub based_on: RevisionPair,
    pub content_sha256: String,
    #[serde(default)]
    pub payload_kind: DevicePayloadKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broker_output: Option<BrokerOutputMarker>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeviceRevision {
    pub revision: u64,
    #[serde(default)]
    pub content_sha256: String,
    pub source_generation: u64,
    #[serde(default)]
    pub source_object_path: String,
    #[serde(default)]
    pub accepted_object_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_file_name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Tombstone {
    pub deleted_by: DeviceSide,
    pub deleted_at_revision: u64,
    pub event_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalStroke {
    pub stroke_id: String,
    pub snapshot: StrokeSnapshot,
    pub last_modified_by: DeviceSide,
    pub source_revisions: RevisionPair,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tombstone: Option<Tombstone>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedView {
    pub object_path: String,
    pub content_sha256: String,
    pub source_revisions: RevisionPair,
    pub event_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreservedInput {
    pub event_id: String,
    pub source: DeviceSide,
    pub object_path: String,
    pub preserved_path: String,
    #[serde(default)]
    pub competing_preserved_paths: Vec<String>,
    pub source_generation: u64,
    pub source_revision: u64,
    pub content_sha256: String,
    pub based_on: RevisionPair,
    pub current_revisions: RevisionPair,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalDocumentState {
    pub schema_version: u32,
    pub document_id: String,
    pub original_object_path: String,
    pub original_pdf_sha256: String,
    pub original_file_name: String,
    #[serde(default)]
    pub original_page_count: usize,
    pub state_revision: u64,
    pub boox: DeviceRevision,
    pub supernote: DeviceRevision,
    pub last_common_revisions: RevisionPair,
    #[serde(default)]
    pub processed_event_ids: BTreeSet<String>,
    #[serde(default)]
    pub strokes: BTreeMap<String, CanonicalStroke>,
    #[serde(default)]
    pub source_generations: BTreeMap<String, u64>,
    #[serde(default)]
    pub generated_views: BTreeMap<String, GeneratedView>,
    #[serde(default)]
    pub conflicts: Vec<PreservedInput>,
}

impl CanonicalDocumentState {
    pub fn revisions(&self) -> RevisionPair {
        RevisionPair {
            boox: self.boox.revision,
            supernote: self.supernote.revision,
        }
    }

    pub fn device(&self, side: DeviceSide) -> &DeviceRevision {
        match side {
            DeviceSide::Boox => &self.boox,
            DeviceSide::Supernote => &self.supernote,
        }
    }

    pub fn device_mut(&mut self, side: DeviceSide) -> &mut DeviceRevision {
        match side {
            DeviceSide::Boox => &mut self.boox,
            DeviceSide::Supernote => &mut self.supernote,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProcessOutcome {
    Applied {
        document_id: String,
        event_id: String,
        destination_path: String,
        destination_generation: u64,
        source_revisions: RevisionPair,
    },
    Duplicate {
        document_id: String,
        event_id: String,
    },
    IgnoredBrokerOutput {
        document_id: String,
        event_id: String,
    },
    IgnoredStaleSource {
        document_id: String,
        event_id: String,
    },
    Conflict {
        document_id: String,
        event_id: String,
        preserved_path: String,
        current_revisions: RevisionPair,
        based_on: RevisionPair,
    },
}
