use inkbridge_broker::{
    sha256_hex, BrokerOutputMarker, DeviceSide, RevisionPair, StorageEvent, BROKER_PRODUCER,
    EVENT_SCHEMA_VERSION,
};
use serde::Deserialize;
use std::collections::BTreeMap;

pub const STORAGE_FINALIZED_EVENT_TYPE: &str = "google.cloud.storage.object.v1.finalized";

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudStorageObjectData {
    pub bucket: String,
    pub name: String,
    pub generation: String,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventTranslation {
    Process(StorageEvent),
    Register(RegistrationEvent),
    Ignore { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistrationEvent {
    pub event_id: String,
    pub object_path: String,
    pub source_generation: u64,
    pub original_file_name: String,
}

pub fn translate_storage_finalized_event(
    expected_bucket: &str,
    headers: &BTreeMap<String, String>,
    body: &[u8],
) -> Result<EventTranslation, String> {
    let event_type = header(headers, "ce-type")
        .ok_or_else(|| "Eventarc request is missing ce-type".to_owned())?;
    if event_type != STORAGE_FINALIZED_EVENT_TYPE {
        return Ok(EventTranslation::Ignore {
            reason: format!("unsupported CloudEvent type {event_type}"),
        });
    }
    let event_id = header(headers, "ce-id")
        .ok_or_else(|| "Eventarc request is missing ce-id".to_owned())?
        .to_owned();
    let object: CloudStorageObjectData =
        serde_json::from_slice(body).map_err(|error| format!("invalid Eventarc body: {error}"))?;
    if object.bucket != expected_bucket {
        return Ok(EventTranslation::Ignore {
            reason: format!(
                "event is for bucket {}, not {expected_bucket}",
                object.bucket
            ),
        });
    }
    let source_generation = object
        .generation
        .parse::<u64>()
        .map_err(|error| format!("invalid Cloud Storage generation: {error}"))?;
    if object.name.starts_with("Staging/") {
        if !object
            .metadata
            .get("inkbridge-register-original")
            .is_some_and(|value| value.eq_ignore_ascii_case("true"))
        {
            return Ok(EventTranslation::Ignore {
                reason: format!(
                    "staging object {} is not marked for document registration",
                    object.name
                ),
            });
        }
        let original_file_name = object
            .metadata
            .get("inkbridge-original-file-name")
            .map(String::as_str)
            .filter(|value| !value.trim().is_empty())
            .or_else(|| object.name.rsplit('/').next())
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "registration object has no usable file name".to_owned())?
            .to_owned();
        return Ok(EventTranslation::Register(RegistrationEvent {
            event_id,
            object_path: object.name,
            source_generation,
            original_file_name,
        }));
    }
    let source = if object.name.starts_with("BOOX_Folder/") {
        DeviceSide::Boox
    } else if object.name.starts_with("Supernote_Folder/") {
        DeviceSide::Supernote
    } else {
        return Ok(EventTranslation::Ignore {
            reason: format!("object {} is outside both device folders", object.name),
        });
    };
    let document_id = required_metadata(&object.metadata, "inkbridge-document-id")?.to_owned();
    let generated_by_broker = object
        .metadata
        .get("inkbridge-generated-by")
        .is_some_and(|value| value == BROKER_PRODUCER);
    let (source_revision, based_on, broker_output) = if generated_by_broker {
        let revisions = parse_revisions(required_metadata(
            &object.metadata,
            "inkbridge-source-revisions",
        )?)?;
        let marker_event_id = required_metadata(&object.metadata, "inkbridge-event-id")?.to_owned();
        (
            revisions.get(source),
            revisions,
            Some(BrokerOutputMarker {
                producer: BROKER_PRODUCER.to_owned(),
                event_id: marker_event_id,
                document_id: document_id.clone(),
                source_revisions: revisions,
            }),
        )
    } else {
        let source_revision = required_metadata(&object.metadata, "inkbridge-source-revision")?
            .parse::<u64>()
            .map_err(|error| format!("invalid source revision: {error}"))?;
        let based_on = RevisionPair {
            boox: required_metadata(&object.metadata, "inkbridge-based-on-boox")?
                .parse()
                .map_err(|error| format!("invalid BOOX base revision: {error}"))?,
            supernote: required_metadata(&object.metadata, "inkbridge-based-on-supernote")?
                .parse()
                .map_err(|error| format!("invalid Supernote base revision: {error}"))?,
        };
        (source_revision, based_on, None)
    };
    let content_sha256 = object
        .metadata
        .get("inkbridge-content-sha256")
        .cloned()
        .unwrap_or_default();
    Ok(EventTranslation::Process(StorageEvent {
        schema_version: EVENT_SCHEMA_VERSION,
        event_id,
        document_id,
        source,
        object_path: object.name,
        source_generation,
        source_revision,
        based_on,
        content_sha256,
        broker_output,
    }))
}

pub fn hydrate_content_hash(event: &mut StorageEvent, bytes: &[u8]) -> Result<(), String> {
    let actual = sha256_hex(bytes);
    if event.content_sha256.is_empty() {
        event.content_sha256 = actual;
        return Ok(());
    }
    if event.content_sha256 != actual {
        return Err(format!(
            "event metadata content hash {} does not match object bytes {actual}",
            event.content_sha256
        ));
    }
    Ok(())
}

fn header<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn required_metadata<'a>(
    metadata: &'a BTreeMap<String, String>,
    name: &str,
) -> Result<&'a str, String> {
    metadata
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("Cloud Storage object metadata is missing {name}"))
}

fn parse_revisions(value: &str) -> Result<RevisionPair, String> {
    let (boox, supernote) = value
        .split_once(':')
        .ok_or_else(|| "broker revision metadata must use boox:supernote".to_owned())?;
    Ok(RevisionPair {
        boox: boox
            .parse()
            .map_err(|error| format!("invalid generated BOOX revision: {error}"))?,
        supernote: supernote
            .parse()
            .map_err(|error| format!("invalid generated Supernote revision: {error}"))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn headers() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("ce-id".to_owned(), "event-1".to_owned()),
            (
                "ce-type".to_owned(),
                STORAGE_FINALIZED_EVENT_TYPE.to_owned(),
            ),
        ])
    }

    #[test]
    fn translates_device_upload_metadata_without_using_filename_identity() {
        let body = serde_json::to_vec(&json!({
            "bucket": "private-sync",
            "name": "BOOX_Folder/renamed/document.pdf",
            "generation": "17",
            "metadata": {
                "inkbridge-document-id": "inkbridge-doc-v1-stable",
                "inkbridge-source-revision": "3",
                "inkbridge-based-on-boox": "2",
                "inkbridge-based-on-supernote": "4"
            }
        }))
        .unwrap();
        let EventTranslation::Process(event) =
            translate_storage_finalized_event("private-sync", &headers(), &body).unwrap()
        else {
            panic!("expected process event")
        };
        assert_eq!(event.document_id, "inkbridge-doc-v1-stable");
        assert_eq!(event.source_revision, 3);
        assert_eq!(
            event.based_on,
            RevisionPair {
                boox: 2,
                supernote: 4
            }
        );
    }

    #[test]
    fn reconstructs_a_trusted_loop_marker_for_broker_output() {
        let body = serde_json::to_vec(&json!({
            "bucket": "private-sync",
            "name": "Supernote_Folder/doc/incoming/ops.json",
            "generation": "18",
            "metadata": {
                "inkbridge-generated-by": "inkbridge-broker",
                "inkbridge-event-id": "source-event",
                "inkbridge-document-id": "doc",
                "inkbridge-source-revisions": "5:7",
                "inkbridge-content-sha256": "abc"
            }
        }))
        .unwrap();
        let EventTranslation::Process(event) =
            translate_storage_finalized_event("private-sync", &headers(), &body).unwrap()
        else {
            panic!("expected process event")
        };
        assert_eq!(event.source_revision, 7);
        assert_eq!(event.broker_output.unwrap().event_id, "source-event");
    }

    #[test]
    fn translates_only_explicitly_marked_staging_objects_to_registration() {
        let unmarked = serde_json::to_vec(&json!({
            "bucket": "private-sync",
            "name": "Staging/original.pdf",
            "generation": "20"
        }))
        .unwrap();
        assert!(matches!(
            translate_storage_finalized_event("private-sync", &headers(), &unmarked).unwrap(),
            EventTranslation::Ignore { .. }
        ));

        let marked = serde_json::to_vec(&json!({
            "bucket": "private-sync",
            "name": "Staging/upload-20.pdf",
            "generation": "20",
            "metadata": {
                "inkbridge-register-original": "true",
                "inkbridge-original-file-name": "daily reading.pdf"
            }
        }))
        .unwrap();
        let EventTranslation::Register(registration) =
            translate_storage_finalized_event("private-sync", &headers(), &marked).unwrap()
        else {
            panic!("expected registration event")
        };
        assert_eq!(registration.event_id, "event-1");
        assert_eq!(registration.source_generation, 20);
        assert_eq!(registration.original_file_name, "daily reading.pdf");
    }
}
