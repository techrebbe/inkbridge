use inkbridge_broker::{
    sha256_hex, supernote_manifest_path, Broker, BrokerStorage, DevicePayloadKind, DeviceSide,
    MemoryStorage, RevisionPair, StorageEvent, BROKER_PRODUCER, EVENT_SCHEMA_VERSION,
};
use inkbridge_convert::{geometry_fingerprint, Manifest, Operation, StrokeSnapshot};
use inkbridge_folder_transport::{
    BooxManifestBuilder, BuiltBooxManifest, CloudFolder, CloudObject, DocumentFolders,
    DocumentTransportState, FileObservation, FolderTransport, NativeBooxManifestBuilder,
    PendingUpload, SideTransportState, TransportAction, TransportState,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};
use tempfile::tempdir;

const GENERATED_BY: &str = "inkbridge-generated-by";
const DOCUMENT_ID: &str = "inkbridge-document-id";
const SOURCE_REVISIONS: &str = "inkbridge-source-revisions";
const SOURCE_REVISION: &str = "inkbridge-source-revision";
const SOURCE_VIEW_SHA256: &str = "inkbridge-source-view-sha256";
const SOURCE_LOCAL_ID: &str = "inkbridge-source-local-id";
const SOURCE_PAGE_INDEX: &str = "inkbridge-source-page-index";
const CONTENT_SHA256: &str = "inkbridge-content-sha256";

#[derive(Default)]
struct FakeCloud {
    objects: Mutex<Vec<(CloudObject, Vec<u8>)>>,
    next_generation: Mutex<u64>,
    downloads: Mutex<u64>,
}

impl FakeCloud {
    fn put(&self, path: &str, bytes: Vec<u8>, metadata: BTreeMap<String, String>) {
        let mut next = self.next_generation.lock().unwrap();
        *next += 1;
        self.objects.lock().unwrap().push((
            CloudObject {
                path: path.to_owned(),
                generation: *next,
                size: bytes.len() as u64,
                metadata,
            },
            bytes,
        ));
    }
}

impl CloudFolder for FakeCloud {
    fn list(&self, prefix: &str) -> Result<Vec<CloudObject>, String> {
        Ok(self
            .objects
            .lock()
            .unwrap()
            .iter()
            .filter(|(object, _)| object.path.starts_with(prefix))
            .map(|(object, _)| object.clone())
            .collect())
    }

    fn upload_create(
        &self,
        local_path: &Path,
        object_path: &str,
        metadata: &BTreeMap<String, String>,
    ) -> Result<CloudObject, String> {
        if let Some((existing, _)) = self
            .objects
            .lock()
            .unwrap()
            .iter()
            .find(|(object, _)| object.path == object_path)
        {
            return Ok(existing.clone());
        }
        let bytes = fs::read(local_path).unwrap();
        self.put(object_path, bytes, metadata.clone());
        Ok(self.objects.lock().unwrap().last().unwrap().0.clone())
    }

    fn download(&self, object: &CloudObject, destination: &Path) -> Result<(), String> {
        *self.downloads.lock().unwrap() += 1;
        let objects = self.objects.lock().unwrap();
        let (_, bytes) = objects
            .iter()
            .find(|(candidate, _)| {
                candidate.path == object.path && candidate.generation == object.generation
            })
            .ok_or_else(|| "missing fake object".to_owned())?;
        fs::write(destination, bytes).map_err(|error| error.to_string())
    }
}

struct MutatingDownloadCloud {
    inner: FakeCloud,
    boox_pdf: PathBuf,
    replacement: Vec<u8>,
}

impl CloudFolder for MutatingDownloadCloud {
    fn list(&self, prefix: &str) -> Result<Vec<CloudObject>, String> {
        self.inner.list(prefix)
    }

    fn upload_create(
        &self,
        local_path: &Path,
        object_path: &str,
        metadata: &BTreeMap<String, String>,
    ) -> Result<CloudObject, String> {
        self.inner.upload_create(local_path, object_path, metadata)
    }

    fn download(&self, object: &CloudObject, destination: &Path) -> Result<(), String> {
        self.inner.download(object, destination)?;
        fs::write(&self.boox_pdf, &self.replacement).map_err(|error| error.to_string())
    }
}

struct FakeBuilder;

impl BooxManifestBuilder for FakeBuilder {
    fn build(&self, pdf: &Path, _baselines: &[PathBuf]) -> Result<BuiltBooxManifest, String> {
        Ok(BuiltBooxManifest {
            bytes: b"{\"schemaVersion\":1}\n".to_vec(),
            source_pdf_sha256: sha256_hex(&fs::read(pdf).unwrap()),
        })
    }
}

struct HashingBuilder;

impl BooxManifestBuilder for HashingBuilder {
    fn build(&self, pdf: &Path, _baselines: &[PathBuf]) -> Result<BuiltBooxManifest, String> {
        Ok(BuiltBooxManifest {
            bytes: b"{\"schemaVersion\":1}\n".to_vec(),
            source_pdf_sha256: sha256_hex(&fs::read(pdf).unwrap()),
        })
    }
}

struct StaleHashBuilder;

impl BooxManifestBuilder for StaleHashBuilder {
    fn build(&self, _pdf: &Path, _baselines: &[PathBuf]) -> Result<BuiltBooxManifest, String> {
        Ok(BuiltBooxManifest {
            bytes: b"{\"schemaVersion\":1}\n".to_vec(),
            source_pdf_sha256: sha256_hex(b"different bytes observed before conversion"),
        })
    }
}

struct MutatingBaselineBuilder;

impl BooxManifestBuilder for MutatingBaselineBuilder {
    fn build(&self, pdf: &Path, baselines: &[PathBuf]) -> Result<BuiltBooxManifest, String> {
        let replacement = String::from_utf8(fs::read(&baselines[0]).unwrap())
            .unwrap()
            .replace("s1", "s2");
        fs::write(&baselines[0], replacement).unwrap();
        Ok(BuiltBooxManifest {
            bytes: b"{\"schemaVersion\":1}\n".to_vec(),
            source_pdf_sha256: sha256_hex(&fs::read(pdf).unwrap()),
        })
    }
}

fn mapping(root: &Path) -> DocumentFolders {
    DocumentFolders {
        document_id: format!("inkbridge-doc-v1-{}", "a".repeat(64)),
        original_file_name: "book.pdf".to_owned(),
        boox_pdf: root.join("boox/book.pdf"),
        supernote_export_directory: root.join("supernote/outgoing"),
        supernote_incoming_directory: root.join("supernote/incoming"),
    }
}

fn native_export() -> Vec<u8> {
    native_export_page(0, "s1")
}

fn native_export_page(page_index: u32, source_uuid: &str) -> Vec<u8> {
    format!(
        r#"{{"sourceFileName":"book.pdf","pageIndex":{page_index},"strokes":[{{"sourceUuid":"{source_uuid}","sourceKey":"{source_uuid}","layerNum":0,"thickness":2,"penColor":0,"penType":16,"samples":[[0.1,0.2,900],[0.2,0.3,1000]]}}]}}"#
    )
    .into_bytes()
}

fn native_export_at(page_index: u32, source_uuid: &str, boox: u64, supernote: u64) -> Vec<u8> {
    format!(
        r#"{{"sourceFileName":"book.pdf","basedOn":{{"boox":{boox},"supernote":{supernote}}},"pageIndex":{page_index},"strokes":[{{"sourceUuid":"{source_uuid}","sourceKey":"{source_uuid}","layerNum":0,"thickness":2,"penColor":0,"penType":16,"samples":[[0.1,0.2,900],[0.2,0.3,1000]]}}]}}"#
    )
    .into_bytes()
}

fn generated_metadata(
    document_id: &str,
    revisions: &str,
    bytes: &[u8],
) -> BTreeMap<String, String> {
    BTreeMap::from([
        (GENERATED_BY.to_owned(), BROKER_PRODUCER.to_owned()),
        (DOCUMENT_ID.to_owned(), document_id.to_owned()),
        (SOURCE_REVISIONS.to_owned(), revisions.to_owned()),
        (CONTENT_SHA256.to_owned(), sha256_hex(bytes)),
    ])
}

fn path_key(path: &Path) -> String {
    let value = path
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/");
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value
    }
}

fn supernote_page_id(document_id: &str, page_index: u32) -> String {
    sha256_hex(format!("{document_id}\0supernote-page\0{page_index}").as_bytes())
}

fn accepted_supernote_upload(
    cloud: &FakeCloud,
    document: &DocumentFolders,
    page_index: u32,
    revision: u64,
    bytes: Vec<u8>,
) {
    let source_hash = sha256_hex(&bytes);
    cloud.put(
        &format!(
            "Supernote_Folder/{}/uploads/supernote-r{revision}.json",
            document.document_id
        ),
        bytes,
        BTreeMap::from([
            (DOCUMENT_ID.to_owned(), document.document_id.clone()),
            (SOURCE_REVISION.to_owned(), revision.to_string()),
            (SOURCE_VIEW_SHA256.to_owned(), source_hash),
            (
                SOURCE_LOCAL_ID.to_owned(),
                supernote_page_id(&document.document_id, page_index),
            ),
            (SOURCE_PAGE_INDEX.to_owned(), page_index.to_string()),
        ]),
    );
}

#[test]
fn supernote_export_upload_is_revisioned_and_duplicate_scan_is_idempotent() {
    let root = tempdir().unwrap();
    let document = mapping(root.path());
    fs::create_dir_all(&document.supernote_export_directory).unwrap();
    let export = document.supernote_export_directory.join("page-1.json");
    fs::write(&export, native_export_at(0, "s1", 0, 0)).unwrap();
    let cloud = FakeCloud::default();
    let builder = FakeBuilder;
    let transport = FolderTransport::new(&cloud, &builder, Duration::ZERO);
    let mut state = TransportState::empty();

    let first = transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();
    assert!(matches!(
        first.actions.as_slice(),
        [TransportAction::Uploaded {
            side: DeviceSide::Supernote,
            source_revision: 1,
            ..
        }]
    ));
    assert!(state.documents[&document.document_id]
        .supernote
        .pending
        .is_some());
    assert_eq!(
        cloud.objects.lock().unwrap()[0]
            .0
            .metadata
            .get(SOURCE_LOCAL_ID),
        Some(&sha256_hex(
            format!("{}\0supernote-page\0{}", document.document_id, 0).as_bytes()
        ))
    );
    assert_eq!(
        cloud.objects.lock().unwrap()[0]
            .0
            .metadata
            .get(SOURCE_PAGE_INDEX)
            .map(String::as_str),
        Some("0")
    );

    let second = transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();
    assert!(second.actions.is_empty());
    assert_eq!(cloud.objects.lock().unwrap().len(), 1);
}

#[test]
fn sibling_page_export_rebases_across_a_supernote_only_revision() {
    let root = tempdir().unwrap();
    let document = mapping(root.path());
    fs::create_dir_all(&document.supernote_export_directory).unwrap();
    let export = document.supernote_export_directory.join("page-0002.json");
    fs::write(&export, native_export_at(1, "s2", 0, 0)).unwrap();

    let cloud = FakeCloud::default();
    accepted_supernote_upload(&cloud, &document, 0, 1, native_export_at(0, "s1", 0, 0));
    let mut state = TransportState::empty();
    state.documents.insert(
        document.document_id.clone(),
        DocumentTransportState {
            revisions: RevisionPair {
                boox: 0,
                supernote: 1,
            },
            ..Default::default()
        },
    );
    let builder = FakeBuilder;
    let transport = FolderTransport::new(&cloud, &builder, Duration::ZERO);

    let report = transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();

    assert!(report.actions.iter().any(|action| matches!(
        action,
        TransportAction::Uploaded {
            side: DeviceSide::Supernote,
            source_revision: 2,
            ..
        }
    )));
    let objects = cloud.objects.lock().unwrap();
    let uploaded = objects
        .iter()
        .map(|(object, _)| object)
        .find(|object| {
            object
                .metadata
                .get(SOURCE_REVISION)
                .is_some_and(|value| value == "2")
        })
        .unwrap();
    assert_eq!(
        uploaded
            .metadata
            .get("inkbridge-based-on-supernote")
            .map(String::as_str),
        Some("1")
    );
    assert_eq!(
        uploaded.metadata.get(SOURCE_PAGE_INDEX).map(String::as_str),
        Some("1")
    );
}

#[test]
fn stale_same_page_export_does_not_rebase_across_a_newer_revision() {
    let root = tempdir().unwrap();
    let document = mapping(root.path());
    fs::create_dir_all(&document.supernote_export_directory).unwrap();
    let export = document.supernote_export_directory.join("page-0001.json");
    fs::write(&export, native_export_at(0, "stale", 0, 0)).unwrap();

    let cloud = FakeCloud::default();
    accepted_supernote_upload(
        &cloud,
        &document,
        0,
        1,
        native_export_at(0, "accepted", 0, 0),
    );
    let mut state = TransportState::empty();
    state.documents.insert(
        document.document_id.clone(),
        DocumentTransportState {
            revisions: RevisionPair {
                boox: 0,
                supernote: 1,
            },
            ..Default::default()
        },
    );
    let builder = FakeBuilder;
    let transport = FolderTransport::new(&cloud, &builder, Duration::ZERO);

    let report = transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();

    assert!(report.actions.iter().any(|action| matches!(
        action,
        TransportAction::Deferred {
            side: DeviceSide::Supernote,
            reason,
        } if reason.contains("same Supernote page changed")
    )));
    assert!(cloud.objects.lock().unwrap().iter().all(|(object, _)| {
        object
            .metadata
            .get(SOURCE_REVISION)
            .is_none_or(|revision| revision != "2")
    }));
}

#[test]
fn broker_manifest_delivery_advances_revision_and_clears_pending() {
    let root = tempdir().unwrap();
    let document = mapping(root.path());
    let cloud = FakeCloud::default();
    let manifest = b"{\"operations\":[]}".to_vec();
    cloud.put(
        &format!(
            "Supernote_Folder/{}/incoming/event.operations.json",
            document.document_id
        ),
        manifest.clone(),
        generated_metadata(&document.document_id, "1:0", &manifest),
    );
    let mut state = TransportState::empty();
    state.documents.insert(
        document.document_id.clone(),
        DocumentTransportState {
            boox: SideTransportState {
                pending: Some(PendingUpload {
                    object_path: "upload".to_owned(),
                    generation: 1,
                    source_revision: 1,
                    based_on: RevisionPair::default(),
                    local_path: "book".to_owned(),
                    local_content_sha256: "source".to_owned(),
                    payload_content_sha256: "payload".to_owned(),
                }),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let builder = FakeBuilder;
    let transport = FolderTransport::new(&cloud, &builder, Duration::ZERO);

    let report = transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();
    let delivered_path = match report.actions.first() {
        Some(TransportAction::Delivered {
            side: DeviceSide::Supernote,
            local_path,
            ..
        }) => local_path,
        other => panic!("expected Supernote delivery, got {other:?}"),
    };
    assert_eq!(fs::read(delivered_path).unwrap(), manifest);
    assert!(delivered_path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("r00000000000000000001-r00000000000000000000-g"));
    let current = &state.documents[&document.document_id];
    assert_eq!(
        current.revisions,
        RevisionPair {
            boox: 1,
            supernote: 0
        }
    );
    assert!(current.boox.pending.is_none());
    assert_eq!(current.boox.accepted_local_hashes["book"], "source");

    let again = transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();
    assert!(again.actions.is_empty());
}

#[test]
fn missing_unacknowledged_manifest_is_redelivered_from_cloud() {
    let root = tempdir().unwrap();
    let document = mapping(root.path());
    let cloud = FakeCloud::default();
    let manifest = b"{\"manifestId\":\"recover-me\",\"operations\":[]}".to_vec();
    cloud.put(
        &format!(
            "Supernote_Folder/{}/incoming/recover.operations.json",
            document.document_id
        ),
        manifest.clone(),
        generated_metadata(&document.document_id, "1:0", &manifest),
    );
    let builder = FakeBuilder;
    let transport = FolderTransport::new(&cloud, &builder, Duration::ZERO);
    let mut state = TransportState::empty();

    let first = transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();
    let delivered_path = first
        .actions
        .iter()
        .find_map(|action| match action {
            TransportAction::Delivered {
                side: DeviceSide::Supernote,
                local_path,
                ..
            } => Some(local_path.clone()),
            _ => None,
        })
        .unwrap();
    fs::remove_file(&delivered_path).unwrap();

    let recovered = transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();
    assert!(recovered.actions.iter().any(|action| matches!(
        action,
        TransportAction::Delivered {
            side: DeviceSide::Supernote,
            local_path,
            ..
        } if local_path == &delivered_path
    )));
    assert_eq!(fs::read(&delivered_path).unwrap(), manifest);
    assert_eq!(*cloud.downloads.lock().unwrap(), 2);

    let delivery_id = sha256_hex(&manifest);
    let acknowledgements = document.supernote_acknowledged_directory();
    fs::create_dir_all(&acknowledgements).unwrap();
    fs::write(
        acknowledgements.join(format!("{delivery_id}.ack.json")),
        format!(
            "{{\"schemaVersion\":1,\"deliveryId\":\"{delivery_id}\",\"documentId\":\"{}\"}}\n",
            document.document_id
        ),
    )
    .unwrap();
    fs::remove_file(&delivered_path).unwrap();

    let acknowledged = transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();
    assert!(!acknowledged
        .actions
        .iter()
        .any(|action| matches!(action, TransportAction::Delivered { .. })));
    assert!(!delivered_path.exists());
    assert_eq!(*cloud.downloads.lock().unwrap(), 2);
}

#[test]
fn unacknowledged_supernote_delivery_blocks_page_export_upload() {
    let root = tempdir().unwrap();
    let document = mapping(root.path());
    fs::create_dir_all(&document.supernote_export_directory).unwrap();
    let export = document.supernote_export_directory.join("page-0001.json");
    fs::write(&export, native_export_at(0, "s1", 0, 0)).unwrap();

    let cloud = FakeCloud::default();
    let manifest = b"{\"manifestId\":\"from-boox\",\"operations\":[]}".to_vec();
    cloud.put(
        &format!(
            "Supernote_Folder/{}/incoming/from-boox.operations.json",
            document.document_id
        ),
        manifest.clone(),
        generated_metadata(&document.document_id, "1:0", &manifest),
    );
    let builder = FakeBuilder;
    let transport = FolderTransport::new(&cloud, &builder, Duration::ZERO);
    let mut state = TransportState::empty();

    let blocked = transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();
    assert!(blocked.actions.iter().any(|action| matches!(
        action,
        TransportAction::Deferred {
            side: DeviceSide::Supernote,
            reason,
        } if reason.contains("applied and acknowledged")
    )));
    assert!(cloud.objects.lock().unwrap().iter().all(|(object, _)| {
        !object.path.starts_with(&format!(
            "Supernote_Folder/{}/uploads/",
            document.document_id
        ))
    }));

    let delivery_id = sha256_hex(&manifest);
    let acknowledgements = document.supernote_acknowledged_directory();
    fs::create_dir_all(&acknowledgements).unwrap();
    fs::write(
        acknowledgements.join(format!("{delivery_id}.ack.json")),
        format!(
            "{{\"schemaVersion\":1,\"deliveryId\":\"{delivery_id}\",\"documentId\":\"{}\"}}\n",
            document.document_id
        ),
    )
    .unwrap();

    let stale = transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();
    assert!(stale.actions.iter().any(|action| matches!(
        action,
        TransportAction::Deferred {
            side: DeviceSide::Supernote,
            reason,
        } if reason.contains("captured at revisions 0:0")
    )));
    assert!(cloud.objects.lock().unwrap().iter().all(|(object, _)| {
        !object.path.starts_with(&format!(
            "Supernote_Folder/{}/uploads/",
            document.document_id
        ))
    }));

    fs::write(&export, native_export_at(0, "s1", 1, 0)).unwrap();
    // The first scan observes the replacement; the second sees the finalized
    // export at its new causal frontier.
    transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();
    let resumed = transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();
    assert!(resumed.actions.iter().any(|action| matches!(
        action,
        TransportAction::Uploaded {
            side: DeviceSide::Supernote,
            source_revision: 1,
            ..
        }
    )));
}

#[test]
fn broker_manifest_delivery_names_preserve_causal_revision_order() {
    let root = tempdir().unwrap();
    let document = mapping(root.path());
    let cloud = FakeCloud::default();
    let first = b"{\"manifestId\":\"first\",\"operations\":[]}".to_vec();
    let second = b"{\"manifestId\":\"second\",\"operations\":[]}".to_vec();
    // Cloud generations are intentionally reversed relative to broker
    // revisions. Delivery must still preserve causal revision order.
    cloud.put(
        &format!(
            "Supernote_Folder/{}/incoming/a-newer-generation-first.operations.json",
            document.document_id
        ),
        second.clone(),
        generated_metadata(&document.document_id, "2:0", &second),
    );
    cloud.put(
        &format!(
            "Supernote_Folder/{}/incoming/z-older-generation-second.operations.json",
            document.document_id
        ),
        first.clone(),
        generated_metadata(&document.document_id, "1:0", &first),
    );
    let builder = FakeBuilder;
    let transport = FolderTransport::new(&cloud, &builder, Duration::ZERO);
    let mut state = TransportState::empty();

    transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();

    let mut delivered = fs::read_dir(&document.supernote_incoming_directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    delivered.sort();
    assert_eq!(delivered.len(), 2);
    assert_eq!(fs::read(&delivered[0]).unwrap(), first);
    assert_eq!(fs::read(&delivered[1]).unwrap(), second);
}

#[test]
fn broker_manifest_delivery_waits_for_a_missing_predecessor() {
    let root = tempdir().unwrap();
    let document = mapping(root.path());
    let cloud = FakeCloud::default();
    let second = b"{\"manifestId\":\"second\",\"operations\":[]}".to_vec();
    cloud.put(
        &format!(
            "Supernote_Folder/{}/incoming/second.operations.json",
            document.document_id
        ),
        second,
        generated_metadata(
            &document.document_id,
            "2:0",
            b"{\"manifestId\":\"second\",\"operations\":[]}",
        ),
    );
    let builder = FakeBuilder;
    let transport = FolderTransport::new(&cloud, &builder, Duration::ZERO);
    let mut state = TransportState::empty();

    let report = transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();

    assert!(report.actions.iter().any(|action| matches!(
        action,
        TransportAction::Deferred {
            side: DeviceSide::Supernote,
            reason,
        } if reason.contains("waiting for predecessor 1:0")
    )));
    assert!(fs::read_dir(&document.supernote_incoming_directory)
        .unwrap()
        .next()
        .is_none());
    assert_eq!(
        state.documents[&document.document_id].revisions,
        RevisionPair::default()
    );
}

#[test]
fn boox_update_uploads_compact_manifest_instead_of_large_pdf() {
    let root = tempdir().unwrap();
    let document = mapping(root.path());
    fs::create_dir_all(document.boox_pdf.parent().unwrap()).unwrap();
    fs::create_dir_all(&document.supernote_export_directory).unwrap();
    fs::write(&document.boox_pdf, vec![7_u8; 2 * 1024 * 1024]).unwrap();
    let export = document.supernote_export_directory.join("page.json");
    let export_bytes = native_export();
    fs::write(&export, &export_bytes).unwrap();
    let export_hash = sha256_hex(&export_bytes);
    let export_key = path_key(&export);
    let mut state = TransportState::empty();
    state.documents.insert(
        document.document_id.clone(),
        DocumentTransportState {
            supernote: SideTransportState {
                uploaded_local_hashes: BTreeMap::from([(export_key.clone(), export_hash.clone())]),
                accepted_local_hashes: BTreeMap::from([(export_key, export_hash)]),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let cloud = FakeCloud::default();
    let builder = FakeBuilder;
    let transport = FolderTransport::new(&cloud, &builder, Duration::ZERO);

    let report = transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();
    let uploaded = report
        .actions
        .iter()
        .find_map(|action| match action {
            TransportAction::Uploaded {
                side: DeviceSide::Boox,
                uploaded_bytes,
                ..
            } => Some(*uploaded_bytes),
            _ => None,
        })
        .unwrap();
    assert!(uploaded < 1024, "the full BOOX PDF was uploaded");
    let objects = cloud.objects.lock().unwrap();
    assert_eq!(objects.len(), 1);
    assert_eq!(
        objects[0]
            .0
            .metadata
            .get("inkbridge-payload-kind")
            .map(String::as_str),
        Some("boox_operation_manifest")
    );
    assert_eq!(objects[0].1, b"{\"schemaVersion\":1}\n");
}

#[test]
fn boox_upload_waits_when_an_accepted_baseline_file_is_missing() {
    let root = tempdir().unwrap();
    let document = mapping(root.path());
    fs::create_dir_all(document.boox_pdf.parent().unwrap()).unwrap();
    fs::create_dir_all(&document.supernote_export_directory).unwrap();
    fs::write(&document.boox_pdf, b"edited BOOX view").unwrap();
    let present = document.supernote_export_directory.join("page-1.json");
    let missing = document.supernote_export_directory.join("page-2.json");
    let present_bytes = native_export();
    let missing_bytes = native_export();
    fs::write(&present, &present_bytes).unwrap();
    fs::write(&missing, &missing_bytes).unwrap();
    let present_key = path_key(&present);
    let missing_key = path_key(&missing);
    fs::remove_file(&missing).unwrap();
    let accepted = BTreeMap::from([
        (present_key.clone(), sha256_hex(&present_bytes)),
        (missing_key.clone(), sha256_hex(&missing_bytes)),
    ]);
    let mut state = TransportState::empty();
    state.documents.insert(
        document.document_id.clone(),
        DocumentTransportState {
            supernote: SideTransportState {
                uploaded_local_hashes: accepted.clone(),
                accepted_local_hashes: accepted,
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let cloud = FakeCloud::default();
    let builder = FakeBuilder;
    let transport = FolderTransport::new(&cloud, &builder, Duration::ZERO);

    let report = transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();

    assert!(report.actions.iter().any(|action| matches!(
        action,
        TransportAction::Deferred {
            side: DeviceSide::Boox,
            reason,
        } if reason.contains("accepted Supernote baseline files are missing")
            && reason.contains(&missing_key)
    )));
    assert!(!cloud.objects.lock().unwrap().iter().any(|(object, _)| {
        object
            .path
            .starts_with(&format!("BOOX_Folder/{}/uploads/", document.document_id))
    }));
}

#[test]
fn lost_checkpoint_recovery_restores_accepted_baselines_from_cloud() {
    let root = tempdir().unwrap();
    let document = mapping(root.path());
    fs::create_dir_all(document.boox_pdf.parent().unwrap()).unwrap();
    fs::create_dir_all(&document.supernote_export_directory).unwrap();
    fs::write(&document.boox_pdf, b"edited BOOX view").unwrap();
    let present = document.supernote_export_directory.join("page-1.json");
    let present_bytes = native_export_page(0, "present");
    fs::write(&present, &present_bytes).unwrap();
    let present_hash = sha256_hex(&present_bytes);
    let missing_bytes = native_export_page(1, "missing");
    let missing_hash = sha256_hex(&missing_bytes);
    let cloud = FakeCloud::default();
    for (revision, bytes, hash) in [
        (1_u64, missing_bytes.clone(), missing_hash.clone()),
        (2, present_bytes, present_hash),
    ] {
        cloud.put(
            &format!(
                "Supernote_Folder/{}/uploads/supernote-r{revision}.json",
                document.document_id
            ),
            bytes,
            BTreeMap::from([
                (DOCUMENT_ID.to_owned(), document.document_id.clone()),
                (SOURCE_REVISION.to_owned(), revision.to_string()),
                (SOURCE_VIEW_SHA256.to_owned(), hash),
            ]),
        );
    }
    let missing_identity =
        sha256_hex(format!("{}\0supernote-page\0{}", document.document_id, 1).as_bytes());
    let corrupted_snapshot = document
        .supernote_export_directory
        .parent()
        .unwrap()
        .join(".inkbridge-accepted")
        .join(format!(
            "r{:020}-{missing_identity}-{missing_hash}.json",
            1_u64
        ));
    fs::create_dir_all(corrupted_snapshot.parent().unwrap()).unwrap();
    fs::write(&corrupted_snapshot, b"corrupted cache entry").unwrap();
    let mut state = TransportState::empty();
    state.documents.insert(
        document.document_id.clone(),
        DocumentTransportState {
            revisions: RevisionPair {
                boox: 0,
                supernote: 2,
            },
            ..Default::default()
        },
    );
    let builder = FakeBuilder;
    let transport = FolderTransport::new(&cloud, &builder, Duration::ZERO);

    let report = transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();

    assert!(!report.actions.iter().any(|action| matches!(
        action,
        TransportAction::Deferred {
            side: DeviceSide::Boox,
            reason,
        } if reason.contains("accepted Supernote baseline files are missing")
    )));
    let accepted = &state.documents[&document.document_id]
        .supernote
        .accepted_local_hashes;
    assert_eq!(accepted.len(), 2);
    assert!(accepted.values().any(|hash| hash == &missing_hash));
    assert_eq!(fs::read(&corrupted_snapshot).unwrap(), missing_bytes);
    for (path, expected_hash) in accepted {
        assert!(path.contains(".inkbridge-accepted"));
        assert_eq!(sha256_hex(&fs::read(path).unwrap()), *expected_hash);
    }
    assert!(cloud.objects.lock().unwrap().iter().any(|(object, _)| {
        object
            .path
            .starts_with(&format!("BOOX_Folder/{}/uploads/", document.document_id))
    }));
}

#[test]
fn recovery_replaces_a_stale_accepted_path_after_export_rename() {
    let root = tempdir().unwrap();
    let document = mapping(root.path());
    fs::create_dir_all(&document.supernote_export_directory).unwrap();
    let old_path = document.supernote_export_directory.join("old-page.json");
    let new_path = document
        .supernote_export_directory
        .join("renamed-page.json");
    let bytes = native_export();
    fs::write(&old_path, &bytes).unwrap();
    let old_key = path_key(&old_path);
    fs::rename(&old_path, &new_path).unwrap();
    let new_key = path_key(&new_path);
    let content_hash = sha256_hex(&bytes);
    let cloud = FakeCloud::default();
    cloud.put(
        &format!(
            "Supernote_Folder/{}/uploads/supernote-r1.json",
            document.document_id
        ),
        bytes.clone(),
        BTreeMap::from([
            (DOCUMENT_ID.to_owned(), document.document_id.clone()),
            (SOURCE_REVISION.to_owned(), "1".to_owned()),
            (SOURCE_VIEW_SHA256.to_owned(), content_hash.clone()),
            (SOURCE_LOCAL_ID.to_owned(), sha256_hex(old_key.as_bytes())),
        ]),
    );
    let mut state = TransportState::empty();
    state.documents.insert(
        document.document_id.clone(),
        DocumentTransportState {
            revisions: RevisionPair {
                boox: 0,
                supernote: 1,
            },
            supernote: SideTransportState {
                accepted_local_hashes: BTreeMap::from([(old_key.clone(), content_hash.clone())]),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let builder = FakeBuilder;
    let transport = FolderTransport::new(&cloud, &builder, Duration::ZERO);

    transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();

    let accepted = &state.documents[&document.document_id]
        .supernote
        .accepted_local_hashes;
    assert!(!accepted.contains_key(&old_key));
    assert!(!accepted.contains_key(&new_key));
    assert_eq!(accepted.len(), 1);
    let (snapshot, snapshot_hash) = accepted.first_key_value().unwrap();
    assert!(snapshot.contains(".inkbridge-accepted"));
    assert_eq!(snapshot_hash, &content_hash);
    assert_eq!(fs::read(snapshot).unwrap(), bytes);
}

#[test]
fn recovery_retires_an_older_page_identity_after_rename_and_edit() {
    let root = tempdir().unwrap();
    let document = mapping(root.path());
    fs::create_dir_all(document.boox_pdf.parent().unwrap()).unwrap();
    fs::create_dir_all(&document.supernote_export_directory).unwrap();
    fs::write(&document.boox_pdf, b"edited BOOX view").unwrap();
    let old_path = document.supernote_export_directory.join("old-page.json");
    let new_path = document
        .supernote_export_directory
        .join("renamed-page.json");
    let old_bytes = native_export();
    let new_bytes = String::from_utf8(old_bytes.clone())
        .unwrap()
        .replace("s1", "s2")
        .into_bytes();
    fs::write(&old_path, &old_bytes).unwrap();
    let old_key = path_key(&old_path);
    fs::rename(&old_path, &new_path).unwrap();
    fs::write(&new_path, &new_bytes).unwrap();
    let new_key = path_key(&new_path);
    let old_hash = sha256_hex(&old_bytes);
    let new_hash = sha256_hex(&new_bytes);
    let page_identity =
        sha256_hex(format!("{}\0supernote-page\0{}", document.document_id, 0).as_bytes());
    let cloud = FakeCloud::default();
    for (revision, bytes, hash, source_identity, page_index) in [
        (
            1_u64,
            old_bytes,
            old_hash.clone(),
            sha256_hex(old_key.as_bytes()),
            None,
        ),
        (
            2_u64,
            new_bytes.clone(),
            new_hash.clone(),
            page_identity.clone(),
            Some("0".to_owned()),
        ),
    ] {
        let mut metadata = BTreeMap::from([
            (DOCUMENT_ID.to_owned(), document.document_id.clone()),
            (SOURCE_REVISION.to_owned(), revision.to_string()),
            (SOURCE_VIEW_SHA256.to_owned(), hash),
            (SOURCE_LOCAL_ID.to_owned(), source_identity),
        ]);
        if let Some(page_index) = page_index {
            metadata.insert(SOURCE_PAGE_INDEX.to_owned(), page_index);
        }
        cloud.put(
            &format!(
                "Supernote_Folder/{}/uploads/supernote-r{revision}.json",
                document.document_id
            ),
            bytes,
            metadata,
        );
    }
    let mut state = TransportState::empty();
    state.documents.insert(
        document.document_id.clone(),
        DocumentTransportState {
            revisions: RevisionPair {
                boox: 0,
                supernote: 2,
            },
            supernote: SideTransportState {
                accepted_local_hashes: BTreeMap::from([(old_key.clone(), old_hash)]),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let builder = FakeBuilder;
    let transport = FolderTransport::new(&cloud, &builder, Duration::ZERO);

    let report = transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();

    let accepted = &state.documents[&document.document_id]
        .supernote
        .accepted_local_hashes;
    assert!(!accepted.contains_key(&new_key));
    assert_eq!(accepted.len(), 1);
    let (snapshot, snapshot_hash) = accepted.first_key_value().unwrap();
    assert!(snapshot.contains(".inkbridge-accepted"));
    assert_eq!(snapshot_hash, &new_hash);
    assert_eq!(fs::read(snapshot).unwrap(), new_bytes);
    assert!(!accepted.contains_key(&old_key));
    assert!(report.actions.iter().any(|action| matches!(
        action,
        TransportAction::Uploaded {
            side: DeviceSide::Boox,
            ..
        }
    )));
    assert_eq!(*cloud.downloads.lock().unwrap(), 2);

    transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();
    assert_eq!(
        *cloud.downloads.lock().unwrap(),
        2,
        "the checkpointed legacy page identity should avoid another download"
    );
}

#[test]
fn simultaneous_local_edits_use_the_cached_accepted_supernote_baseline() {
    let root = tempdir().unwrap();
    let document = mapping(root.path());
    fs::create_dir_all(document.boox_pdf.parent().unwrap()).unwrap();
    fs::create_dir_all(&document.supernote_export_directory).unwrap();
    fs::write(&document.boox_pdf, b"concurrent BOOX edit").unwrap();
    let accepted_bytes = native_export_at(0, "accepted", 0, 0);
    let cloud = FakeCloud::default();
    accepted_supernote_upload(&cloud, &document, 0, 1, accepted_bytes);
    let current_export = document.supernote_export_directory.join("page-0001.json");
    fs::write(&current_export, native_export_at(0, "concurrent", 1, 1)).unwrap();
    let mut state = TransportState::empty();
    state.documents.insert(
        document.document_id.clone(),
        DocumentTransportState {
            revisions: RevisionPair {
                boox: 1,
                supernote: 1,
            },
            ..Default::default()
        },
    );
    let builder = FakeBuilder;
    let transport = FolderTransport::new(&cloud, &builder, Duration::ZERO);

    let report = transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();

    assert!(report.actions.iter().any(|action| matches!(
        action,
        TransportAction::Uploaded {
            side: DeviceSide::Boox,
            source_revision: 2,
            ..
        }
    )));
    assert!(report.actions.iter().any(|action| matches!(
        action,
        TransportAction::Uploaded {
            side: DeviceSide::Supernote,
            source_revision: 2,
            ..
        }
    )));
    let accepted = &state.documents[&document.document_id]
        .supernote
        .accepted_local_hashes;
    assert_eq!(accepted.len(), 1);
    assert!(accepted
        .keys()
        .all(|path| path.contains(".inkbridge-accepted")));
}

#[test]
fn boox_skip_rehashes_a_settled_file_when_size_and_mtime_look_unchanged() {
    let root = tempdir().unwrap();
    let document = mapping(root.path());
    fs::create_dir_all(document.boox_pdf.parent().unwrap()).unwrap();
    fs::create_dir_all(&document.supernote_export_directory).unwrap();
    let old_bytes = b"old BOOX bytes";
    let new_bytes = b"new BOOX bytes";
    assert_eq!(old_bytes.len(), new_bytes.len());
    fs::write(&document.boox_pdf, new_bytes).unwrap();
    let export = document.supernote_export_directory.join("page.json");
    let export_bytes = native_export();
    fs::write(&export, &export_bytes).unwrap();

    let old_hash = sha256_hex(old_bytes);
    let new_hash = sha256_hex(new_bytes);
    let boox_key = path_key(&document.boox_pdf);
    let export_key = path_key(&export);
    let export_hash = sha256_hex(&export_bytes);
    let metadata = fs::metadata(&document.boox_pdf).unwrap();
    let modified_millis = metadata
        .modified()
        .unwrap()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let mut state = TransportState {
        documents: BTreeMap::from([(
            document.document_id.clone(),
            DocumentTransportState {
                boox: SideTransportState {
                    uploaded_local_hashes: BTreeMap::from([(boox_key.clone(), old_hash.clone())]),
                    ..Default::default()
                },
                supernote: SideTransportState {
                    uploaded_local_hashes: BTreeMap::from([(
                        export_key.clone(),
                        export_hash.clone(),
                    )]),
                    accepted_local_hashes: BTreeMap::from([(export_key, export_hash)]),
                    ..Default::default()
                },
                ..Default::default()
            },
        )]),
        ..TransportState::empty()
    };
    state.observations.insert(
        boox_key,
        FileObservation {
            size: metadata.len(),
            modified_unix_millis: modified_millis,
            first_seen_unix_millis: 0,
            content_sha256: Some(old_hash),
        },
    );
    let cloud = FakeCloud::default();
    let builder = HashingBuilder;
    let transport = FolderTransport::new(&cloud, &builder, Duration::ZERO);

    let first = transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();

    assert!(first.actions.iter().any(|action| matches!(
        action,
        TransportAction::Deferred {
            side: DeviceSide::Boox,
            reason,
        } if reason.contains("without new filesystem metadata")
    )));
    assert!(!cloud.objects.lock().unwrap().iter().any(|(object, _)| {
        object
            .path
            .starts_with(&format!("BOOX_Folder/{}/uploads/", document.document_id))
    }));

    let second = transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();
    assert!(second.actions.iter().any(|action| matches!(
        action,
        TransportAction::Uploaded {
            side: DeviceSide::Boox,
            source_revision: 1,
            ..
        }
    )));
    assert_eq!(
        state.documents[&document.document_id]
            .boox
            .uploaded_local_hashes[&path_key(&document.boox_pdf)],
        new_hash
    );
}

#[test]
fn supernote_same_metadata_replacement_restarts_settling_before_upload() {
    let root = tempdir().unwrap();
    let document = mapping(root.path());
    fs::create_dir_all(&document.supernote_export_directory).unwrap();
    let export = document.supernote_export_directory.join("page.json");
    let old_bytes = native_export();
    let new_bytes = String::from_utf8(old_bytes.clone())
        .unwrap()
        .replace("s1", "s2")
        .into_bytes();
    assert_eq!(old_bytes.len(), new_bytes.len());
    fs::write(&export, &new_bytes).unwrap();
    let metadata = fs::metadata(&export).unwrap();
    let export_key = path_key(&export);
    let mut state = TransportState::empty();
    state.observations.insert(
        export_key.clone(),
        FileObservation {
            size: metadata.len(),
            modified_unix_millis: metadata
                .modified()
                .unwrap()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
            first_seen_unix_millis: 1,
            content_sha256: Some(sha256_hex(&old_bytes)),
        },
    );
    let cloud = FakeCloud::default();
    let builder = FakeBuilder;
    let transport = FolderTransport::new(&cloud, &builder, Duration::ZERO);
    let scan_now = SystemTime::UNIX_EPOCH + Duration::from_secs(34_567);

    let report = transport
        .sync_document(&document, &mut state, scan_now)
        .unwrap();

    assert!(report.actions.iter().any(|action| matches!(
        action,
        TransportAction::Deferred {
            side: DeviceSide::Supernote,
            reason,
        } if reason.contains("without new filesystem metadata")
    )));
    assert!(cloud.objects.lock().unwrap().is_empty());
    assert!(state.observations[&export_key].first_seen_unix_millis > 34_567_000);
}

#[test]
fn boox_upload_is_deferred_when_post_conversion_hash_does_not_match_source() {
    let root = tempdir().unwrap();
    let document = mapping(root.path());
    fs::create_dir_all(document.boox_pdf.parent().unwrap()).unwrap();
    fs::create_dir_all(&document.supernote_export_directory).unwrap();
    fs::write(&document.boox_pdf, b"current BOOX bytes").unwrap();
    let export = document.supernote_export_directory.join("page.json");
    let export_bytes = native_export();
    fs::write(&export, &export_bytes).unwrap();
    let export_key = path_key(&export);
    let export_hash = sha256_hex(&export_bytes);
    let boox_key = path_key(&document.boox_pdf);
    let metadata = fs::metadata(&document.boox_pdf).unwrap();
    let mut state = TransportState {
        documents: BTreeMap::from([(
            document.document_id.clone(),
            DocumentTransportState {
                supernote: SideTransportState {
                    uploaded_local_hashes: BTreeMap::from([(
                        export_key.clone(),
                        export_hash.clone(),
                    )]),
                    accepted_local_hashes: BTreeMap::from([(export_key, export_hash)]),
                    ..Default::default()
                },
                ..Default::default()
            },
        )]),
        ..TransportState::empty()
    };
    state.observations.insert(
        boox_key.clone(),
        FileObservation {
            size: metadata.len(),
            modified_unix_millis: metadata
                .modified()
                .unwrap()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
            first_seen_unix_millis: 1,
            content_sha256: Some(sha256_hex(b"current BOOX bytes")),
        },
    );
    let cloud = FakeCloud::default();
    let builder = StaleHashBuilder;
    let transport = FolderTransport::new(&cloud, &builder, Duration::ZERO);
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(12_345);

    let report = transport.sync_document(&document, &mut state, now).unwrap();

    assert!(report.actions.iter().any(|action| matches!(
        action,
        TransportAction::Deferred {
            side: DeviceSide::Boox,
            reason,
        } if reason.contains("content changed")
    )));
    assert!(!cloud.objects.lock().unwrap().iter().any(|(object, _)| {
        object
            .path
            .starts_with(&format!("BOOX_Folder/{}/uploads/", document.document_id))
    }));
    assert!(state.documents[&document.document_id]
        .boox
        .pending
        .is_none());
    assert!(state.observations[&boox_key].first_seen_unix_millis > 12_345_000);
}

#[test]
fn boox_upload_is_deferred_when_an_accepted_baseline_changes_during_conversion() {
    let root = tempdir().unwrap();
    let document = mapping(root.path());
    fs::create_dir_all(document.boox_pdf.parent().unwrap()).unwrap();
    fs::create_dir_all(&document.supernote_export_directory).unwrap();
    fs::write(&document.boox_pdf, b"current BOOX bytes").unwrap();
    let export = document.supernote_export_directory.join("page.json");
    let export_bytes = native_export();
    fs::write(&export, &export_bytes).unwrap();
    let export_key = path_key(&export);
    let export_hash = sha256_hex(&export_bytes);
    let mut state = TransportState {
        documents: BTreeMap::from([(
            document.document_id.clone(),
            DocumentTransportState {
                supernote: SideTransportState {
                    uploaded_local_hashes: BTreeMap::from([(
                        export_key.clone(),
                        export_hash.clone(),
                    )]),
                    accepted_local_hashes: BTreeMap::from([(export_key.clone(), export_hash)]),
                    ..Default::default()
                },
                ..Default::default()
            },
        )]),
        ..TransportState::empty()
    };
    let cloud = FakeCloud::default();
    let builder = MutatingBaselineBuilder;
    let transport = FolderTransport::new(&cloud, &builder, Duration::ZERO);
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(23_456);

    let report = transport.sync_document(&document, &mut state, now).unwrap();

    assert!(report.actions.iter().any(|action| matches!(
        action,
        TransportAction::Deferred {
            side: DeviceSide::Boox,
            reason,
        } if reason.contains("Supernote baseline") && reason.contains("changed")
    )));
    assert!(!cloud.objects.lock().unwrap().iter().any(|(object, _)| {
        object
            .path
            .starts_with(&format!("BOOX_Folder/{}/uploads/", document.document_id))
    }));
    assert!(state.documents[&document.document_id]
        .boox
        .pending
        .is_none());
    assert!(state.observations[&export_key].first_seen_unix_millis > 23_456_000);
}

#[test]
fn generated_boox_view_never_overwrites_an_unpublished_local_edit() {
    let root = tempdir().unwrap();
    let document = mapping(root.path());
    fs::create_dir_all(document.boox_pdf.parent().unwrap()).unwrap();
    fs::write(&document.boox_pdf, b"local edit").unwrap();
    let cloud = FakeCloud::default();
    let generated = b"broker view".to_vec();
    cloud.put(
        &format!("BOOX_Folder/{}/book.pdf", document.document_id),
        generated.clone(),
        generated_metadata(&document.document_id, "0:1", &generated),
    );
    let builder = FakeBuilder;
    let transport = FolderTransport::new(&cloud, &builder, Duration::ZERO);
    let mut state = TransportState::empty();

    let report = transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();
    assert!(report.actions.iter().any(|action| matches!(
        action,
        TransportAction::Deferred {
            side: DeviceSide::Boox,
            ..
        }
    )));
    assert_eq!(fs::read(&document.boox_pdf).unwrap(), b"local edit");
}

#[test]
fn generated_boox_view_never_overwrites_an_edit_created_during_download() {
    let root = tempdir().unwrap();
    let document = mapping(root.path());
    fs::create_dir_all(document.boox_pdf.parent().unwrap()).unwrap();
    let installed = b"previous broker view".to_vec();
    fs::write(&document.boox_pdf, &installed).unwrap();
    let generated = b"next broker view".to_vec();
    let cloud = MutatingDownloadCloud {
        inner: FakeCloud::default(),
        boox_pdf: document.boox_pdf.clone(),
        replacement: b"local edit created during download".to_vec(),
    };
    cloud.inner.put(
        &format!("BOOX_Folder/{}/book.pdf", document.document_id),
        generated.clone(),
        generated_metadata(&document.document_id, "0:1", &generated),
    );
    let builder = FakeBuilder;
    let transport = FolderTransport::new(&cloud, &builder, Duration::from_secs(60));
    let mut state = TransportState::empty();
    state.documents.insert(
        document.document_id.clone(),
        DocumentTransportState {
            boox: SideTransportState {
                delivered_content_sha256: Some(sha256_hex(&installed)),
                ..Default::default()
            },
            ..Default::default()
        },
    );

    let report = transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();

    assert!(report.actions.iter().any(|action| matches!(
        action,
        TransportAction::Deferred {
            side: DeviceSide::Boox,
            reason,
        } if reason.contains("changed while it was downloading")
    )));
    assert!(!report
        .actions
        .iter()
        .any(|action| matches!(action, TransportAction::Delivered { .. })));
    assert_eq!(
        fs::read(&document.boox_pdf).unwrap(),
        b"local edit created during download"
    );
    assert_eq!(
        state.documents[&document.document_id].revisions,
        RevisionPair::default()
    );
    assert!(state.documents[&document.document_id]
        .delivered_generations
        .is_empty());
}

#[test]
fn already_installed_boox_view_recovers_a_lost_checkpoint() {
    let root = tempdir().unwrap();
    let document = mapping(root.path());
    fs::create_dir_all(document.boox_pdf.parent().unwrap()).unwrap();
    let generated = b"broker view already on disk".to_vec();
    fs::write(&document.boox_pdf, &generated).unwrap();
    let cloud = FakeCloud::default();
    cloud.put(
        &format!("BOOX_Folder/{}/book.pdf", document.document_id),
        generated.clone(),
        generated_metadata(&document.document_id, "0:1", &generated),
    );
    let builder = FakeBuilder;
    let transport = FolderTransport::new(&cloud, &builder, Duration::ZERO);
    let mut state = TransportState::empty();

    let report = transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();
    assert!(report.actions.iter().any(|action| matches!(
        action,
        TransportAction::Delivered {
            side: DeviceSide::Boox,
            ..
        }
    )));
    assert!(!report.actions.iter().any(|action| matches!(
        action,
        TransportAction::Deferred {
            side: DeviceSide::Boox,
            ..
        }
    )));
    let recovered = &state.documents[&document.document_id];
    assert_eq!(
        recovered.revisions,
        RevisionPair {
            boox: 0,
            supernote: 1
        }
    );
    assert_eq!(
        recovered.boox.delivered_content_sha256.as_deref(),
        Some(sha256_hex(&generated).as_str())
    );
}

#[test]
fn acknowledged_boox_upload_recovers_after_checkpoint_loss_without_allocating_r2() {
    let root = tempdir().unwrap();
    let document = mapping(root.path());
    fs::create_dir_all(document.boox_pdf.parent().unwrap()).unwrap();
    fs::create_dir_all(&document.supernote_export_directory).unwrap();
    fs::write(&document.boox_pdf, b"edited BOOX view").unwrap();
    let export = document.supernote_export_directory.join("page.json");
    let export_bytes = native_export();
    fs::write(&export, &export_bytes).unwrap();
    let export_hash = sha256_hex(&export_bytes);
    let export_key = path_key(&export);
    let checkpoint = TransportState {
        documents: BTreeMap::from([(
            document.document_id.clone(),
            DocumentTransportState {
                supernote: SideTransportState {
                    uploaded_local_hashes: BTreeMap::from([(
                        export_key.clone(),
                        export_hash.clone(),
                    )]),
                    accepted_local_hashes: BTreeMap::from([(export_key, export_hash)]),
                    ..Default::default()
                },
                ..Default::default()
            },
        )]),
        ..TransportState::empty()
    };
    let cloud = FakeCloud::default();
    let builder = FakeBuilder;
    let transport = FolderTransport::new(&cloud, &builder, Duration::ZERO);
    let mut state = checkpoint.clone();

    transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();
    assert!(state.documents[&document.document_id]
        .boox
        .pending
        .is_some());

    let manifest = b"{\"operations\":[]}".to_vec();
    cloud.put(
        &format!(
            "Supernote_Folder/{}/incoming/accepted.operations.json",
            document.document_id
        ),
        manifest.clone(),
        generated_metadata(&document.document_id, "1:0", &manifest),
    );

    // Simulate losing the checkpoint written after the immutable r1 upload.
    state = checkpoint;
    transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();

    let uploads = cloud
        .objects
        .lock()
        .unwrap()
        .iter()
        .filter(|(object, _)| {
            object
                .path
                .starts_with(&format!("BOOX_Folder/{}/uploads/", document.document_id))
        })
        .count();
    assert_eq!(
        uploads, 1,
        "the accepted r1 source was uploaded again as r2"
    );
    let recovered = &state.documents[&document.document_id];
    assert_eq!(recovered.revisions.boox, 1);
    assert!(recovered.boox.pending.is_none());
    assert_eq!(
        recovered
            .boox
            .accepted_local_hashes
            .get(&path_key(&document.boox_pdf))
            .map(String::as_str),
        Some(sha256_hex(b"edited BOOX view").as_str())
    );
}

#[test]
fn conflict_object_blocks_new_uploads_and_is_reported() {
    let root = tempdir().unwrap();
    let document = mapping(root.path());
    fs::create_dir_all(&document.supernote_export_directory).unwrap();
    fs::write(
        document.supernote_export_directory.join("page.json"),
        native_export(),
    )
    .unwrap();
    let cloud = FakeCloud::default();
    cloud.put(
        &format!("Conflicts/{}/event/incoming.json", document.document_id),
        b"conflict".to_vec(),
        BTreeMap::new(),
    );
    let builder = FakeBuilder;
    let transport = FolderTransport::new(&cloud, &builder, Duration::ZERO);
    let mut state = TransportState::empty();

    let report = transport
        .sync_document(&document, &mut state, SystemTime::now())
        .unwrap();
    assert!(report
        .actions
        .iter()
        .any(|action| matches!(action, TransportAction::Conflict { .. })));
    assert_eq!(state.documents[&document.document_id].conflicts.len(), 1);
}

#[test]
fn failed_scan_does_not_publish_partial_transport_state() {
    let root = tempdir().unwrap();
    let document = mapping(root.path());
    fs::create_dir_all(&document.supernote_export_directory).unwrap();
    fs::write(
        document.supernote_export_directory.join("page.json"),
        b"not a native export",
    )
    .unwrap();
    let cloud = FakeCloud::default();
    let builder = FakeBuilder;
    let transport = FolderTransport::new(&cloud, &builder, Duration::ZERO);
    let mut state = TransportState::empty();
    let original = state.clone();

    assert!(transport
        .sync_document(&document, &mut state, SystemTime::now())
        .is_err());
    assert_eq!(state, original);
}

#[test]
#[ignore = "requires the private real-device fixture directory"]
fn compact_transport_path_matches_the_proven_real_device_manifest() {
    let root = std::env::var_os("INKBRIDGE_REAL_FIXTURE_ROOT")
        .map(PathBuf::from)
        .expect("set INKBRIDGE_REAL_FIXTURE_ROOT to artifacts/dual-device-test");
    let original = fs::read(root.join("Shapiro0146-0153-Supernote-original.pdf")).unwrap();
    let baseline_path = root.join("supernote/InkBridge_Baseline.json");
    let baseline = fs::read(&baseline_path).unwrap();
    let boox_path = root.join("boox/Shapiro0146-0153-NeoReader-Embedded.pdf");
    let expected = fs::read(root.join("return/Shapiro-review-fix-6-test.json")).unwrap();
    let mut storage = MemoryStorage::default();
    let broker = Broker::default();
    let state = broker
        .register_document(
            &mut storage,
            "Shapiro0146-0153-Supernote-original.pdf",
            &original,
        )
        .unwrap();
    let baseline_object_path = format!("Supernote_Folder/{}/baseline.json", state.document_id);
    let baseline_object =
        storage.put_unchecked(&baseline_object_path, baseline.clone(), BTreeMap::new());
    broker
        .process(
            &mut storage,
            &StorageEvent {
                schema_version: EVENT_SCHEMA_VERSION,
                event_id: "transport-real-sn".to_owned(),
                document_id: state.document_id.clone(),
                source: DeviceSide::Supernote,
                object_path: baseline_object_path,
                source_generation: baseline_object.generation,
                source_revision: 1,
                based_on: RevisionPair::default(),
                content_sha256: sha256_hex(&baseline),
                payload_kind: DevicePayloadKind::DeviceView,
                broker_output: None,
            },
        )
        .unwrap();

    let compact = NativeBooxManifestBuilder
        .build(&boox_path, &[baseline_path])
        .unwrap();
    let compact_path = format!(
        "BOOX_Folder/{}/uploads/real.operations.json",
        state.document_id
    );
    let compact_object =
        storage.put_unchecked(&compact_path, compact.bytes.clone(), BTreeMap::new());
    broker
        .process(
            &mut storage,
            &StorageEvent {
                schema_version: EVENT_SCHEMA_VERSION,
                event_id: "transport-real-boox".to_owned(),
                document_id: state.document_id.clone(),
                source: DeviceSide::Boox,
                object_path: compact_path,
                source_generation: compact_object.generation,
                source_revision: 1,
                based_on: RevisionPair {
                    boox: 0,
                    supernote: 1,
                },
                content_sha256: sha256_hex(&compact.bytes),
                payload_kind: DevicePayloadKind::BooxOperationManifest,
                broker_output: None,
            },
        )
        .unwrap();
    let actual = storage
        .read(&supernote_manifest_path(
            &state.document_id,
            "transport-real-boox",
        ))
        .unwrap()
        .unwrap();
    let mut expected: Manifest = serde_json::from_slice(&expected).unwrap();
    normalize_expected_broker_colors(&mut expected);
    let mut expected = serde_json::to_vec_pretty(&expected).unwrap();
    expected.push(b'\n');
    assert_eq!(actual.bytes.as_ref(), expected.as_slice());
}

fn normalize_expected_broker_colors(manifest: &mut Manifest) {
    for operation in &mut manifest.operations {
        match operation {
            Operation::UpsertStroke { before, after, .. } => {
                if let Some(before) = before {
                    normalize_expected_snapshot(before);
                }
                normalize_expected_snapshot(after);
            }
            Operation::DeleteStroke { before, .. } => normalize_expected_snapshot(before),
        }
    }
}

fn normalize_expected_snapshot(snapshot: &mut StrokeSnapshot) {
    let normalized = if snapshot.native_style.pen_color == 0 {
        0
    } else {
        0x9d
    };
    if snapshot.native_style.pen_color != normalized {
        snapshot.native_style.pen_color = normalized;
        snapshot.geometry_fingerprint =
            geometry_fingerprint(&snapshot.native_style, &snapshot.samples);
    }
}
