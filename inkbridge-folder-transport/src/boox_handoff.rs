use crate::{CloudObject, DocumentFolders};
use inkbridge_broker::{
    DevicePayloadKind, DeviceSide, RevisionPair, StorageEvent, BROKER_PRODUCER,
    EVENT_SCHEMA_VERSION,
};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

const DESCRIPTOR_SCHEMA_VERSION: u32 = 1;
const DESCRIPTOR_SUFFIX: &str = ".pdf.inkbridge.json";
const MAX_DESCRIPTOR_BYTES: u64 = 256 * 1024;
const GENERATED_EVENT_ID: &str = "inkbridge-event-id";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BooxHandoffEndpoint {
    document_root: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreparedBooxDelivery {
    pub pdf_path: PathBuf,
    pub descriptor_path: PathBuf,
    pub descriptor_bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FinalizedBooxArtifact {
    pub descriptor_path: PathBuf,
    pub pdf_path: PathBuf,
    pub event: StorageEvent,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BrokerDeliveryDescriptor<'a> {
    schema_version: u32,
    producer: &'a str,
    event_id: &'a str,
    document_id: &'a str,
    original_file_name: &'a str,
    source_revisions: RevisionPair,
    source_generation: u64,
    content_sha256: &'a str,
    pdf_file_name: &'a str,
}

impl BooxHandoffEndpoint {
    pub fn new(root: &Path, document: &DocumentFolders) -> Result<Self, String> {
        validate_document_id(&document.document_id)?;
        validate_file_name(&document.original_file_name, "originalFileName")?;
        Ok(Self {
            document_root: root.join(&document.document_id),
        })
    }

    pub fn prepare_delivery(
        &self,
        document: &DocumentFolders,
        object: &CloudObject,
        revisions: RevisionPair,
        content_sha256: &str,
    ) -> Result<PreparedBooxDelivery, String> {
        validate_sha256(content_sha256, "broker output content hash")?;
        let event_id = object
            .metadata
            .get(GENERATED_EVENT_ID)
            .map(String::as_str)
            .filter(|value| !value.trim().is_empty() && value.len() <= 256)
            .ok_or_else(|| {
                format!(
                    "broker output {} is missing a valid {GENERATED_EVENT_ID}",
                    object.path
                )
            })?;
        let pdf_file_name = format!(
            "broker-b{:020}-s{:020}-g{:020}-{}.pdf",
            revisions.boox,
            revisions.supernote,
            object.generation,
            &content_sha256[..12]
        );
        let incoming = self.document_root.join("incoming");
        let pdf_path = incoming.join(&pdf_file_name);
        let descriptor_path = incoming.join(format!("{pdf_file_name}.inkbridge.json"));
        let descriptor = BrokerDeliveryDescriptor {
            schema_version: DESCRIPTOR_SCHEMA_VERSION,
            producer: BROKER_PRODUCER,
            event_id,
            document_id: &document.document_id,
            original_file_name: &document.original_file_name,
            source_revisions: revisions,
            source_generation: object.generation,
            content_sha256,
            pdf_file_name: &pdf_file_name,
        };
        let mut descriptor_bytes =
            serde_json::to_vec_pretty(&descriptor).map_err(|error| error.to_string())?;
        descriptor_bytes.push(b'\n');
        Ok(PreparedBooxDelivery {
            pdf_path,
            descriptor_path,
            descriptor_bytes,
        })
    }

    pub fn finalized_artifacts(
        &self,
        document: &DocumentFolders,
    ) -> Result<Vec<FinalizedBooxArtifact>, String> {
        let outgoing = self.document_root.join("outgoing");
        let entries = match fs::read_dir(&outgoing) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(format!(
                    "could not inspect BOOX handoff outgoing directory {}: {error}",
                    outgoing.display()
                ))
            }
        };
        let mut artifacts = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| {
                format!(
                    "could not inspect an entry in BOOX handoff outgoing directory {}: {error}",
                    outgoing.display()
                )
            })?;
            let descriptor_path = entry.path();
            let name = descriptor_path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            if !name.ends_with(DESCRIPTOR_SUFFIX) {
                continue;
            }
            let metadata = entry.metadata().map_err(|error| {
                format!(
                    "could not inspect BOOX handoff descriptor {}: {error}",
                    descriptor_path.display()
                )
            })?;
            if !metadata.is_file() {
                return Err(format!(
                    "BOOX handoff descriptor {} is not a regular file",
                    descriptor_path.display()
                ));
            }
            if metadata.len() > MAX_DESCRIPTOR_BYTES {
                return Err(format!(
                    "BOOX handoff descriptor {} exceeds {MAX_DESCRIPTOR_BYTES} bytes",
                    descriptor_path.display()
                ));
            }
            let bytes = fs::read(&descriptor_path).map_err(|error| {
                format!(
                    "could not read BOOX handoff descriptor {}: {error}",
                    descriptor_path.display()
                )
            })?;
            let event: StorageEvent = serde_json::from_slice(&bytes).map_err(|error| {
                format!(
                    "invalid BOOX handoff descriptor {}: {error}",
                    descriptor_path.display()
                )
            })?;
            validate_finalized_event(&event, document)?;
            let pdf_name = name
                .strip_suffix(".inkbridge.json")
                .expect("descriptor suffix was checked above");
            validate_file_name(pdf_name, "finalized BOOX PDF file name")?;
            let object_name = Path::new(&event.object_path)
                .file_name()
                .and_then(|value| value.to_str())
                .ok_or_else(|| {
                    format!(
                        "BOOX handoff descriptor {} has no object file name",
                        descriptor_path.display()
                    )
                })?;
            if object_name != pdf_name {
                return Err(format!(
                    "BOOX handoff descriptor {} names object {} but is paired with {}",
                    descriptor_path.display(),
                    object_name,
                    pdf_name
                ));
            }
            let pdf_path = outgoing.join(pdf_name);
            if !fs::metadata(&pdf_path)
                .map(|metadata| metadata.is_file())
                .unwrap_or(false)
            {
                return Err(format!(
                    "BOOX handoff descriptor {} is missing paired PDF {}",
                    descriptor_path.display(),
                    pdf_path.display()
                ));
            }
            artifacts.push(FinalizedBooxArtifact {
                descriptor_path,
                pdf_path,
                event,
            });
        }
        artifacts.sort_by(|left, right| {
            (
                left.event.source_revision,
                left.event.based_on.boox,
                left.event.based_on.supernote,
                left.event.source_generation,
                &left.descriptor_path,
            )
                .cmp(&(
                    right.event.source_revision,
                    right.event.based_on.boox,
                    right.event.based_on.supernote,
                    right.event.source_generation,
                    &right.descriptor_path,
                ))
        });
        Ok(artifacts)
    }
}

fn validate_finalized_event(
    event: &StorageEvent,
    document: &DocumentFolders,
) -> Result<(), String> {
    if event.schema_version != EVENT_SCHEMA_VERSION {
        return Err(format!(
            "unsupported BOOX handoff event schema {}",
            event.schema_version
        ));
    }
    if event.document_id != document.document_id {
        return Err(format!(
            "BOOX handoff event belongs to {}, not {}",
            event.document_id, document.document_id
        ));
    }
    if event.source != DeviceSide::Boox {
        return Err("BOOX handoff event source is not boox".to_owned());
    }
    if event.payload_kind != DevicePayloadKind::DeviceView || event.broker_output.is_some() {
        return Err("BOOX handoff event is not a finalized device PDF".to_owned());
    }
    if event.event_id.trim().is_empty() || event.event_id.len() > 256 {
        return Err("BOOX handoff event has an invalid eventId".to_owned());
    }
    if event.source_generation == 0 {
        return Err("BOOX handoff event sourceGeneration must be positive".to_owned());
    }
    if event.source_revision != event.based_on.boox + 1 {
        return Err(format!(
            "BOOX handoff source revision {} does not immediately follow basedOn BOOX revision {}",
            event.source_revision, event.based_on.boox
        ));
    }
    validate_sha256(&event.content_sha256, "BOOX handoff content hash")?;
    let prefix = format!("BOOX_Folder/{}/", document.document_id);
    let relative_path = event.object_path.strip_prefix(&prefix).ok_or_else(|| {
        format!(
            "BOOX handoff object path {} is outside {}",
            event.object_path, prefix
        )
    })?;
    if relative_path.is_empty()
        || relative_path.contains(['/', '\\', '\0'])
        || relative_path.chars().any(char::is_control)
    {
        return Err(format!(
            "BOOX handoff object path {} must name one safe PDF directly below {}",
            event.object_path, prefix
        ));
    }
    Ok(())
}

fn validate_document_id(value: &str) -> Result<(), String> {
    let valid = value.strip_prefix("inkbridge-doc-v1-").is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    });
    if valid {
        Ok(())
    } else {
        Err(format!("invalid stable document ID {value}"))
    }
}

fn validate_sha256(value: &str, label: &str) -> Result<(), String> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        Ok(())
    } else {
        Err(format!("invalid {label}"))
    }
}

fn validate_file_name(value: &str, label: &str) -> Result<(), String> {
    let path = Path::new(value);
    if value.is_empty()
        || value.len() > 180
        || path.file_name().and_then(|name| name.to_str()) != Some(value)
        || matches!(value, "." | "..")
        || value
            .chars()
            .any(|character| matches!(character, '/' | '\\' | '\0') || character.is_control())
    {
        Err(format!("invalid {label}"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use tempfile::tempdir;

    fn document() -> DocumentFolders {
        DocumentFolders {
            document_id: format!("inkbridge-doc-v1-{}", "a".repeat(64)),
            original_file_name: "Example.pdf".to_owned(),
            boox_pdf: PathBuf::from("legacy.pdf"),
            supernote_export_directory: PathBuf::from("supernote/outgoing"),
            supernote_incoming_directory: PathBuf::from("supernote/incoming"),
        }
    }

    #[test]
    fn prepares_companion_descriptor_from_broker_metadata() {
        let root = tempdir().unwrap();
        let document = document();
        let endpoint = BooxHandoffEndpoint::new(root.path(), &document).unwrap();
        let hash = "b".repeat(64);
        let object = CloudObject {
            path: format!("BOOX_Folder/{}/Example.pdf", document.document_id),
            generation: 19,
            size: 42,
            metadata: BTreeMap::from([(
                GENERATED_EVENT_ID.to_owned(),
                "broker-event-19".to_owned(),
            )]),
        };
        let prepared = endpoint
            .prepare_delivery(
                &document,
                &object,
                RevisionPair {
                    boox: 2,
                    supernote: 4,
                },
                &hash,
            )
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&prepared.descriptor_bytes).unwrap();
        assert_eq!(value["producer"], BROKER_PRODUCER);
        assert_eq!(value["eventId"], "broker-event-19");
        assert_eq!(value["sourceRevisions"]["boox"], 2);
        assert_eq!(value["sourceRevisions"]["supernote"], 4);
        assert_eq!(value["sourceGeneration"], 19);
        assert_eq!(
            value["pdfFileName"].as_str().unwrap(),
            prepared.pdf_path.file_name().unwrap().to_str().unwrap()
        );
    }

    #[test]
    fn reads_and_validates_finalized_companion_pair() {
        let root = tempdir().unwrap();
        let document = document();
        let endpoint = BooxHandoffEndpoint::new(root.path(), &document).unwrap();
        let outgoing = root.path().join(&document.document_id).join("outgoing");
        fs::create_dir_all(&outgoing).unwrap();
        let bytes = b"NeoReader PDF";
        let pdf_name = "Example__boox-finalized-g1.pdf";
        fs::write(outgoing.join(pdf_name), bytes).unwrap();
        let event = StorageEvent {
            schema_version: EVENT_SCHEMA_VERSION,
            event_id: "boox-finalize-test".to_owned(),
            document_id: document.document_id.clone(),
            source: DeviceSide::Boox,
            object_path: format!("BOOX_Folder/{}/{pdf_name}", document.document_id),
            source_generation: 1,
            source_revision: 3,
            based_on: RevisionPair {
                boox: 2,
                supernote: 4,
            },
            content_sha256: inkbridge_broker::sha256_hex(bytes),
            payload_kind: DevicePayloadKind::DeviceView,
            broker_output: None,
        };
        fs::write(
            outgoing.join(format!("{pdf_name}.inkbridge.json")),
            serde_json::to_vec_pretty(&event).unwrap(),
        )
        .unwrap();

        let artifacts = endpoint.finalized_artifacts(&document).unwrap();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].event, event);
        assert_eq!(artifacts[0].pdf_path, outgoing.join(pdf_name));
    }

    #[test]
    fn rejects_nested_or_traversing_finalized_object_paths() {
        let document = document();
        let event = StorageEvent {
            schema_version: EVENT_SCHEMA_VERSION,
            event_id: "unsafe-path".to_owned(),
            document_id: document.document_id.clone(),
            source: DeviceSide::Boox,
            object_path: format!("BOOX_Folder/{}/../escape.pdf", document.document_id),
            source_generation: 1,
            source_revision: 1,
            based_on: RevisionPair::default(),
            content_sha256: "c".repeat(64),
            payload_kind: DevicePayloadKind::DeviceView,
            broker_output: None,
        };

        assert!(validate_finalized_event(&event, &document)
            .unwrap_err()
            .contains("one safe PDF"));
    }
    #[test]
    fn rejects_descriptor_without_its_paired_pdf() {
        let root = tempdir().unwrap();
        let document = document();
        let endpoint = BooxHandoffEndpoint::new(root.path(), &document).unwrap();
        let outgoing = root.path().join(&document.document_id).join("outgoing");
        fs::create_dir_all(&outgoing).unwrap();
        let pdf_name = "missing.pdf";
        let event = StorageEvent {
            schema_version: EVENT_SCHEMA_VERSION,
            event_id: "missing".to_owned(),
            document_id: document.document_id.clone(),
            source: DeviceSide::Boox,
            object_path: format!("BOOX_Folder/{}/{pdf_name}", document.document_id),
            source_generation: 1,
            source_revision: 1,
            based_on: RevisionPair::default(),
            content_sha256: "c".repeat(64),
            payload_kind: DevicePayloadKind::DeviceView,
            broker_output: None,
        };
        fs::write(
            outgoing.join(format!("{pdf_name}.inkbridge.json")),
            serde_json::to_vec_pretty(&event).unwrap(),
        )
        .unwrap();

        assert!(endpoint
            .finalized_artifacts(&document)
            .unwrap_err()
            .contains("missing paired PDF"));
    }
}
