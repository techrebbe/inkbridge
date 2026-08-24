use crate::{CloudObject, DocumentFolders};
use inkbridge_broker::{
    DevicePayloadKind, DeviceSide, RevisionPair, StorageEvent, BROKER_PRODUCER,
    EVENT_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const DESCRIPTOR_SCHEMA_VERSION: u32 = 1;
const DESCRIPTOR_SUFFIX: &str = ".inkbridge.json";
pub(crate) const MAX_DESCRIPTOR_BYTES: u64 = 256 * 1024;
const GENERATED_EVENT_ID: &str = "inkbridge-event-id";
const INSTALLED_ACKNOWLEDGEMENT_FILE: &str = ".inkbridge-installed.json";
const COMPANION_PRODUCER: &str = "inkbridge-boox-companion";
const RETIREMENT_MARKER_PREFIX: &str = ".inkbridge-retire-";
const RETIREMENT_MARKER_SUFFIX: &str = ".json";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BooxHandoffEndpoint {
    document_root: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreparedBooxDelivery {
    pub event_id: String,
    pub pdf_path: PathBuf,
    pub descriptor_path: PathBuf,
    pub descriptor_bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FinalizedBooxArtifact {
    pub descriptor_path: PathBuf,
    pub payload_path: PathBuf,
    pub event: StorageEvent,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct BrokerDeliveryDescriptor {
    schema_version: u32,
    producer: String,
    event_id: String,
    document_id: String,
    original_file_name: String,
    source_revisions: RevisionPair,
    source_generation: u64,
    content_sha256: String,
    pdf_file_name: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstalledBooxDelivery {
    schema_version: u32,
    producer: String,
    pub event_id: String,
    pub document_id: String,
    pub source_revisions: RevisionPair,
    pub source_generation: u64,
    pub content_sha256: String,
    active_file_name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct FinalizedRetirementMarker {
    schema_version: u32,
    document_id: String,
    event_id: String,
    content_sha256: String,
    pdf_file_name: String,
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
            producer: BROKER_PRODUCER.to_owned(),
            event_id: event_id.to_owned(),
            document_id: document.document_id.clone(),
            original_file_name: document.original_file_name.clone(),
            source_revisions: revisions,
            source_generation: object.generation,
            content_sha256: content_sha256.to_owned(),
            pdf_file_name: pdf_file_name.clone(),
        };
        let mut descriptor_bytes =
            serde_json::to_vec_pretty(&descriptor).map_err(|error| error.to_string())?;
        descriptor_bytes.push(b'\n');
        Ok(PreparedBooxDelivery {
            event_id: event_id.to_owned(),
            pdf_path,
            descriptor_path,
            descriptor_bytes,
        })
    }

    pub fn installed_delivery(
        &self,
        document: &DocumentFolders,
    ) -> Result<Option<InstalledBooxDelivery>, String> {
        let acknowledgement = self.document_root.join(INSTALLED_ACKNOWLEDGEMENT_FILE);
        let metadata = match fs::symlink_metadata(&acknowledgement) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "could not inspect BOOX installed-delivery acknowledgement {}: {error}",
                    acknowledgement.display()
                ))
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!(
                "BOOX installed-delivery acknowledgement {} is not a regular file",
                acknowledgement.display()
            ));
        }
        let bytes = read_bounded_descriptor(&acknowledgement, &metadata)?;
        let installed: InstalledBooxDelivery = serde_json::from_slice(&bytes).map_err(|error| {
            format!(
                "invalid BOOX installed-delivery acknowledgement {}: {error}",
                acknowledgement.display()
            )
        })?;
        validate_installed_delivery(&installed, document)?;
        Ok(Some(installed))
    }

    pub fn retire_superseded_incoming(
        &self,
        document: &DocumentFolders,
        installed: &InstalledBooxDelivery,
    ) -> Result<(), String> {
        validate_installed_delivery(installed, document)?;
        let incoming = self.document_root.join("incoming");
        let entries = match fs::read_dir(&incoming) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(format!(
                    "could not inspect BOOX handoff incoming directory {}: {error}",
                    incoming.display()
                ))
            }
        };
        for entry in entries {
            let descriptor_path = entry
                .map_err(|error| {
                    format!(
                        "could not inspect an entry in BOOX handoff incoming directory {}: {error}",
                        incoming.display()
                    )
                })?
                .path();
            let name = descriptor_path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            if !name.ends_with(DESCRIPTOR_SUFFIX) {
                continue;
            }
            let Ok(delivery) = read_broker_delivery_descriptor(&descriptor_path, name, document)
            else {
                continue;
            };
            let exact_installed = delivery.event_id == installed.event_id
                && delivery.source_revisions == installed.source_revisions
                && delivery.source_generation == installed.source_generation
                && delivery.content_sha256 == installed.content_sha256;
            if exact_installed {
                continue;
            }
            let same_installed_view = delivery.source_revisions == installed.source_revisions
                && delivery.content_sha256 == installed.content_sha256;
            if !same_installed_view
                && !strictly_dominates(installed.source_revisions, delivery.source_revisions)
            {
                continue;
            }
            let pdf_path = incoming.join(&delivery.pdf_file_name);
            validate_regular_pdf_if_exists(&pdf_path, &delivery.content_sha256)?;
            remove_file_and_sync(&pdf_path, &incoming)?;
            remove_file_and_sync(&descriptor_path, &incoming)?;
        }
        Ok(())
    }

    pub fn retire_accepted_artifact(
        &self,
        document: &DocumentFolders,
        artifact: &FinalizedBooxArtifact,
    ) -> Result<(), String> {
        validate_finalized_event(&artifact.event, document)?;
        let pdf_file_name = artifact
            .payload_path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                format!(
                    "finalized BOOX payload {} has no file name",
                    artifact.payload_path.display()
                )
            })?;
        validate_file_name(pdf_file_name, "finalized BOOX payload file name")?;
        let marker = FinalizedRetirementMarker {
            schema_version: 1,
            document_id: document.document_id.clone(),
            event_id: artifact.event.event_id.clone(),
            content_sha256: artifact.event.content_sha256.clone(),
            pdf_file_name: pdf_file_name.to_owned(),
        };
        let outgoing = self.document_root.join("outgoing");
        let marker_path = outgoing.join(retirement_marker_name(&marker.event_id));
        publish_retirement_marker(&marker_path, &marker, &outgoing)?;
        recover_retirement_marker(&marker_path, &outgoing, document)
    }

    fn recover_retirements(&self, document: &DocumentFolders) -> Result<(), String> {
        let outgoing = self.document_root.join("outgoing");
        let entries = match fs::read_dir(&outgoing) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(format!(
                    "could not inspect BOOX handoff outgoing directory {}: {error}",
                    outgoing.display()
                ))
            }
        };
        let mut markers = Vec::new();
        for entry in entries {
            let path = entry
                .map_err(|error| {
                    format!(
                        "could not inspect an entry in BOOX handoff outgoing directory {}: {error}",
                        outgoing.display()
                    )
                })?
                .path();
            if path
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| {
                    name.starts_with(RETIREMENT_MARKER_PREFIX)
                        && name.ends_with(RETIREMENT_MARKER_SUFFIX)
                })
            {
                markers.push(path);
            }
        }
        markers.sort();
        for marker in markers {
            recover_retirement_marker(&marker, &outgoing, document)?;
        }
        Ok(())
    }
    pub fn finalized_artifacts(
        &self,
        document: &DocumentFolders,
    ) -> Result<Vec<FinalizedBooxArtifact>, String> {
        self.recover_retirements(document)?;
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
                .map(str::to_owned)
                .unwrap_or_default();
            if !name.ends_with(DESCRIPTOR_SUFFIX) {
                continue;
            }
            // Folder mirrors may expose a malformed descriptor before its PDF.
            // Validate the cheap metadata/pair contract here, but defer hashing
            // complete PDFs until transport state has filtered acknowledged events.
            // A later hash mismatch is preserved and skipped by the transport.
            if let Ok(artifact) =
                read_finalized_artifact(&outgoing, descriptor_path, &name, document)
            {
                artifacts.push(artifact);
            }
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

fn validate_installed_delivery(
    installed: &InstalledBooxDelivery,
    document: &DocumentFolders,
) -> Result<(), String> {
    if installed.schema_version != 1 {
        return Err(format!(
            "unsupported BOOX installed-delivery acknowledgement schema {}",
            installed.schema_version
        ));
    }
    if installed.producer != COMPANION_PRODUCER {
        return Err(
            "BOOX installed-delivery acknowledgement has an unexpected producer".to_owned(),
        );
    }
    if installed.document_id != document.document_id {
        return Err(format!(
            "BOOX installed-delivery acknowledgement belongs to {}, not {}",
            installed.document_id, document.document_id
        ));
    }
    if installed.event_id.trim().is_empty() || installed.event_id.len() > 256 {
        return Err("BOOX installed-delivery acknowledgement has an invalid eventId".to_owned());
    }
    if installed.source_generation == 0 {
        return Err("BOOX installed-delivery acknowledgement has an invalid generation".to_owned());
    }
    validate_sha256(
        &installed.content_sha256,
        "installed BOOX broker content hash",
    )?;
    validate_file_name(&installed.active_file_name, "active BOOX PDF file name")
}

fn read_broker_delivery_descriptor(
    descriptor_path: &Path,
    name: &str,
    document: &DocumentFolders,
) -> Result<BrokerDeliveryDescriptor, String> {
    let metadata = fs::symlink_metadata(descriptor_path).map_err(|error| {
        format!(
            "could not inspect BOOX broker descriptor {}: {error}",
            descriptor_path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "BOOX broker descriptor {} is not a regular file",
            descriptor_path.display()
        ));
    }
    let bytes = read_bounded_descriptor(descriptor_path, &metadata)?;
    let delivery: BrokerDeliveryDescriptor = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "invalid BOOX broker descriptor {}: {error}",
            descriptor_path.display()
        )
    })?;
    if delivery.schema_version != DESCRIPTOR_SCHEMA_VERSION
        || delivery.producer != BROKER_PRODUCER
        || delivery.document_id != document.document_id
        || delivery.original_file_name != document.original_file_name
        || delivery.event_id.trim().is_empty()
        || delivery.event_id.len() > 256
        || delivery.source_generation == 0
    {
        return Err(format!(
            "BOOX broker descriptor {} does not match its document or protocol",
            descriptor_path.display()
        ));
    }
    validate_sha256(
        &delivery.content_sha256,
        "BOOX broker delivery content hash",
    )?;
    validate_file_name(
        &delivery.pdf_file_name,
        "BOOX broker delivery PDF file name",
    )?;
    if name != format!("{}.inkbridge.json", delivery.pdf_file_name) {
        return Err(format!(
            "BOOX broker descriptor {} does not match PDF {}",
            descriptor_path.display(),
            delivery.pdf_file_name
        ));
    }
    Ok(delivery)
}

fn strictly_dominates(left: RevisionPair, right: RevisionPair) -> bool {
    left != right && left.boox >= right.boox && left.supernote >= right.supernote
}

fn retirement_marker_name(event_id: &str) -> String {
    format!(
        "{RETIREMENT_MARKER_PREFIX}{}{RETIREMENT_MARKER_SUFFIX}",
        inkbridge_broker::sha256_hex(event_id.as_bytes())
    )
}

fn publish_retirement_marker(
    marker_path: &Path,
    marker: &FinalizedRetirementMarker,
    outgoing: &Path,
) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(marker).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_DESCRIPTOR_BYTES {
        return Err("BOOX finalized-retirement marker exceeds the metadata limit".to_owned());
    }
    match fs::symlink_metadata(marker_path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(format!(
                    "BOOX finalized-retirement marker {} is not a regular file",
                    marker_path.display()
                ));
            }
            let existing = read_bounded_descriptor(marker_path, &metadata)?;
            if existing != bytes {
                return Err(format!(
                    "BOOX finalized-retirement marker {} has unexpected content",
                    marker_path.display()
                ));
            }
            return Ok(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "could not inspect BOOX finalized-retirement marker {}: {error}",
                marker_path.display()
            ))
        }
    }
    fs::create_dir_all(outgoing)
        .map_err(|error| format!("could not create {}: {error}", outgoing.display()))?;
    let temporary = marker_path.with_extension("json.part");
    match fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "could not remove stale BOOX retirement marker {}: {error}",
                temporary.display()
            ))
        }
    }
    let mut output = fs::File::create(&temporary)
        .map_err(|error| format!("could not write {}: {error}", temporary.display()))?;
    output
        .write_all(&bytes)
        .and_then(|_| output.flush())
        .and_then(|_| output.sync_all())
        .map_err(|error| format!("could not finalize {}: {error}", temporary.display()))?;
    drop(output);
    let published = crate::transport::publish_create_only(&temporary, marker_path)?;
    if !published {
        let metadata = fs::symlink_metadata(marker_path).map_err(|error| {
            format!(
                "could not inspect concurrently published BOOX retirement marker {}: {error}",
                marker_path.display()
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!(
                "BOOX finalized-retirement marker {} is not a regular file",
                marker_path.display()
            ));
        }
        let existing = read_bounded_descriptor(marker_path, &metadata)?;
        if existing != bytes {
            return Err(format!(
                "BOOX finalized-retirement marker {} has unexpected content",
                marker_path.display()
            ));
        }
    }
    sync_directory(outgoing)
}

fn recover_retirement_marker(
    marker_path: &Path,
    outgoing: &Path,
    document: &DocumentFolders,
) -> Result<(), String> {
    let metadata = fs::symlink_metadata(marker_path).map_err(|error| {
        format!(
            "could not inspect BOOX finalized-retirement marker {}: {error}",
            marker_path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "BOOX finalized-retirement marker {} is not a regular file",
            marker_path.display()
        ));
    }
    let bytes = read_bounded_descriptor(marker_path, &metadata)?;
    let marker: FinalizedRetirementMarker = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "invalid BOOX finalized-retirement marker {}: {error}",
            marker_path.display()
        )
    })?;
    if marker.schema_version != 1
        || marker.document_id != document.document_id
        || marker.event_id.trim().is_empty()
        || marker.event_id.len() > 256
        || marker_path.file_name().and_then(|value| value.to_str())
            != Some(retirement_marker_name(&marker.event_id).as_str())
    {
        return Err(format!(
            "BOOX finalized-retirement marker {} does not match its document or identity",
            marker_path.display()
        ));
    }
    validate_sha256(&marker.content_sha256, "retired BOOX content hash")?;
    validate_file_name(&marker.pdf_file_name, "retired BOOX PDF file name")?;
    let pdf_path = outgoing.join(&marker.pdf_file_name);
    let descriptor_path = outgoing.join(format!("{}.inkbridge.json", marker.pdf_file_name));
    validate_regular_pdf_if_exists(&pdf_path, &marker.content_sha256)?;
    if let Ok(descriptor_metadata) = fs::symlink_metadata(&descriptor_path) {
        if descriptor_metadata.file_type().is_symlink() || !descriptor_metadata.is_file() {
            return Err(format!(
                "acknowledged BOOX descriptor {} is not a regular file",
                descriptor_path.display()
            ));
        }
        let descriptor_bytes = read_bounded_descriptor(&descriptor_path, &descriptor_metadata)?;
        let event: StorageEvent = serde_json::from_slice(&descriptor_bytes).map_err(|error| {
            format!(
                "invalid acknowledged BOOX descriptor {}: {error}",
                descriptor_path.display()
            )
        })?;
        validate_finalized_event(&event, document)?;
        let object_file_name = Path::new(&event.object_path)
            .file_name()
            .and_then(|value| value.to_str());
        if event.event_id != marker.event_id
            || event.content_sha256 != marker.content_sha256
            || object_file_name != Some(marker.pdf_file_name.as_str())
        {
            return Err(format!(
                "acknowledged BOOX descriptor {} does not match its retirement marker",
                descriptor_path.display()
            ));
        }
    }
    remove_file_and_sync(&pdf_path, outgoing)?;
    remove_file_and_sync(&descriptor_path, outgoing)?;
    remove_file_and_sync(marker_path, outgoing)
}

fn validate_regular_pdf_if_exists(path: &Path, expected_hash: &str) -> Result<(), String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("could not inspect {}: {error}", path.display())),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "BOOX handoff PDF {} is not a regular file",
            path.display()
        ));
    }
    let actual_hash = sha256_file(path)?;
    if actual_hash != expected_hash {
        return Err(format!(
            "BOOX handoff PDF {} changed before retirement and was preserved",
            path.display()
        ));
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path)
        .map_err(|error| format!("could not open {} for hashing: {error}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("could not hash {}: {error}", path.display()))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn remove_file_and_sync(path: &Path, directory: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => sync_directory(directory),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("could not remove {}: {error}", path.display())),
    }
}

fn sync_directory(directory: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        match fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(directory)
            .and_then(|file| file.sync_all())
        {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => Ok(()),
            Err(error) => Err(format!("could not sync {}: {error}", directory.display())),
        }
    }
    #[cfg(not(windows))]
    {
        fs::File::open(directory)
            .and_then(|file| file.sync_all())
            .map_err(|error| format!("could not sync {}: {error}", directory.display()))
    }
}
fn read_finalized_artifact(
    outgoing: &Path,
    descriptor_path: PathBuf,
    name: &str,
    document: &DocumentFolders,
) -> Result<FinalizedBooxArtifact, String> {
    let metadata = fs::symlink_metadata(&descriptor_path).map_err(|error| {
        format!(
            "could not inspect BOOX handoff descriptor {}: {error}",
            descriptor_path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "BOOX handoff descriptor {} is not a regular file",
            descriptor_path.display()
        ));
    }
    let bytes = read_bounded_descriptor(&descriptor_path, &metadata)?;
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
    validate_file_name(pdf_name, "finalized BOOX payload file name")?;
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
    let pdf_metadata = fs::symlink_metadata(&pdf_path).map_err(|error| {
        format!(
            "BOOX handoff descriptor {} is missing paired PDF {}: {error}",
            descriptor_path.display(),
            pdf_path.display()
        )
    })?;
    if pdf_metadata.file_type().is_symlink() || !pdf_metadata.is_file() {
        return Err(format!(
            "BOOX handoff descriptor {} is missing a regular paired PDF {}",
            descriptor_path.display(),
            pdf_path.display()
        ));
    }

    Ok(FinalizedBooxArtifact {
        descriptor_path,
        payload_path: pdf_path,
        event,
    })
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
    if !matches!(
        event.payload_kind,
        DevicePayloadKind::DeviceView | DevicePayloadKind::BooxOperationManifest
    ) || event.broker_output.is_some()
    {
        return Err("BOOX handoff event is not a supported finalized payload".to_owned());
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
            "BOOX handoff object path {} must name one safe payload directly below {}",
            event.object_path, prefix
        ));
    }
    Ok(())
}

fn read_bounded_descriptor(path: &Path, metadata: &fs::Metadata) -> Result<Vec<u8>, String> {
    if metadata.len() > MAX_DESCRIPTOR_BYTES {
        return Err(format!(
            "BOOX handoff descriptor {} exceeds {MAX_DESCRIPTOR_BYTES} bytes",
            path.display()
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    fs::File::open(path)
        .map_err(|error| {
            format!(
                "could not read BOOX handoff descriptor {}: {error}",
                path.display()
            )
        })?
        .take(MAX_DESCRIPTOR_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            format!(
                "could not read BOOX handoff descriptor {}: {error}",
                path.display()
            )
        })?;
    if bytes.len() as u64 > MAX_DESCRIPTOR_BYTES {
        return Err(format!(
            "BOOX handoff descriptor {} exceeds {MAX_DESCRIPTOR_BYTES} bytes",
            path.display()
        ));
    }
    Ok(bytes)
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
    fn installed_acknowledgement_retires_only_superseded_incoming_pairs() {
        let root = tempdir().unwrap();
        let document = document();
        let endpoint = BooxHandoffEndpoint::new(root.path(), &document).unwrap();
        let incoming = root.path().join(&document.document_id).join("incoming");
        fs::create_dir_all(&incoming).unwrap();
        let prepare = |event_id: &str, generation: u64, revisions: RevisionPair, bytes: &[u8]| {
            let object = CloudObject {
                path: format!("BOOX_Folder/{}/Example.pdf", document.document_id),
                generation,
                size: bytes.len() as u64,
                metadata: BTreeMap::from([(GENERATED_EVENT_ID.to_owned(), event_id.to_owned())]),
            };
            endpoint
                .prepare_delivery(
                    &document,
                    &object,
                    revisions,
                    &inkbridge_broker::sha256_hex(bytes),
                )
                .unwrap()
        };
        let first = prepare(
            "broker-event-1",
            1,
            RevisionPair {
                boox: 0,
                supernote: 1,
            },
            b"first",
        );
        let second = prepare(
            "broker-event-2",
            2,
            RevisionPair {
                boox: 1,
                supernote: 2,
            },
            b"second",
        );
        let same_view_republish = prepare(
            "broker-event-3",
            3,
            RevisionPair {
                boox: 1,
                supernote: 2,
            },
            b"second",
        );
        let same_revision_conflict = prepare(
            "broker-event-4",
            4,
            RevisionPair {
                boox: 1,
                supernote: 2,
            },
            b"different",
        );
        for (delivery, bytes) in [
            (&first, b"first".as_slice()),
            (&second, b"second".as_slice()),
            (&same_view_republish, b"second".as_slice()),
            (&same_revision_conflict, b"different".as_slice()),
        ] {
            fs::write(&delivery.pdf_path, bytes).unwrap();
            fs::write(&delivery.descriptor_path, &delivery.descriptor_bytes).unwrap();
        }
        fs::write(
            root.path()
                .join(&document.document_id)
                .join(INSTALLED_ACKNOWLEDGEMENT_FILE),
            serde_json::to_vec_pretty(&serde_json::json!({
                "schemaVersion": 1,
                "producer": COMPANION_PRODUCER,
                "eventId": "broker-event-2",
                "documentId": document.document_id,
                "sourceRevisions": {"boox": 1, "supernote": 2},
                "sourceGeneration": 2,
                "contentSha256": inkbridge_broker::sha256_hex(b"second"),
                "activeFileName": "active.pdf"
            }))
            .unwrap(),
        )
        .unwrap();

        let installed = endpoint.installed_delivery(&document).unwrap().unwrap();
        endpoint
            .retire_superseded_incoming(&document, &installed)
            .unwrap();

        assert!(!first.pdf_path.exists());
        assert!(!first.descriptor_path.exists());
        assert!(second.pdf_path.is_file());
        assert!(second.descriptor_path.is_file());
        assert!(!same_view_republish.pdf_path.exists());
        assert!(!same_view_republish.descriptor_path.exists());
        assert!(same_revision_conflict.pdf_path.is_file());
        assert!(same_revision_conflict.descriptor_path.is_file());
    }
    #[test]
    fn retirement_marker_never_overwrites_conflicting_mirrored_metadata() {
        let root = tempdir().unwrap();
        let document = document();
        let outgoing = root.path().join(&document.document_id).join("outgoing");
        fs::create_dir_all(&outgoing).unwrap();
        let marker = FinalizedRetirementMarker {
            schema_version: 1,
            document_id: document.document_id,
            event_id: "accepted-finalization".to_owned(),
            content_sha256: "a".repeat(64),
            pdf_file_name: "Example__boox-finalized-g1.pdf".to_owned(),
        };
        let marker_path = outgoing.join(retirement_marker_name(&marker.event_id));
        fs::write(&marker_path, b"different mirrored marker").unwrap();

        let error = publish_retirement_marker(&marker_path, &marker, &outgoing).unwrap_err();

        assert!(error.contains("unexpected content"));
        assert_eq!(fs::read(marker_path).unwrap(), b"different mirrored marker");
    }

    #[test]
    fn durable_marker_finishes_interrupted_accepted_outgoing_retirement() {
        let root = tempdir().unwrap();
        let document = document();
        let endpoint = BooxHandoffEndpoint::new(root.path(), &document).unwrap();
        let outgoing = root.path().join(&document.document_id).join("outgoing");
        fs::create_dir_all(&outgoing).unwrap();
        let bytes = b"accepted NeoReader PDF";
        let pdf_name = "Example__boox-finalized-g1.pdf";
        let event = StorageEvent {
            schema_version: EVENT_SCHEMA_VERSION,
            event_id: "accepted-finalization".to_owned(),
            document_id: document.document_id.clone(),
            source: DeviceSide::Boox,
            object_path: format!("BOOX_Folder/{}/{pdf_name}", document.document_id),
            source_generation: 1,
            source_revision: 1,
            based_on: RevisionPair::default(),
            content_sha256: inkbridge_broker::sha256_hex(bytes),
            payload_kind: DevicePayloadKind::DeviceView,
            broker_output: None,
        };
        fs::write(outgoing.join(pdf_name), bytes).unwrap();
        fs::write(
            outgoing.join(format!("{pdf_name}.inkbridge.json")),
            serde_json::to_vec_pretty(&event).unwrap(),
        )
        .unwrap();
        let marker = FinalizedRetirementMarker {
            schema_version: 1,
            document_id: document.document_id.clone(),
            event_id: event.event_id.clone(),
            content_sha256: event.content_sha256.clone(),
            pdf_file_name: pdf_name.to_owned(),
        };
        let marker_path = outgoing.join(retirement_marker_name(&event.event_id));
        publish_retirement_marker(&marker_path, &marker, &outgoing).unwrap();

        assert!(endpoint.finalized_artifacts(&document).unwrap().is_empty());
        assert!(fs::read_dir(outgoing).unwrap().next().is_none());
    }
    #[test]
    fn bounded_descriptor_read_rejects_growth_after_metadata_check() {
        let root = tempdir().unwrap();
        let descriptor = root.path().join("delivery.pdf.inkbridge.json");
        fs::write(&descriptor, b"{}").unwrap();
        let stale_metadata = fs::metadata(&descriptor).unwrap();
        fs::write(&descriptor, vec![b'x'; MAX_DESCRIPTOR_BYTES as usize + 1]).unwrap();

        assert!(read_bounded_descriptor(&descriptor, &stale_metadata)
            .unwrap_err()
            .contains("exceeds"));
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
        assert_eq!(artifacts[0].payload_path, outgoing.join(pdf_name));
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
            .contains("one safe payload"));
    }
    #[test]
    fn skips_invalid_or_incomplete_descriptors_without_hashing_complete_pairs() {
        let root = tempdir().unwrap();
        let document = document();
        let endpoint = BooxHandoffEndpoint::new(root.path(), &document).unwrap();
        let outgoing = root.path().join(&document.document_id).join("outgoing");
        fs::create_dir_all(&outgoing).unwrap();
        fs::write(
            outgoing.join("a-malformed.pdf.inkbridge.json"),
            b"{not-json",
        )
        .unwrap();

        let event = |event_id: &str, pdf_name: &str, content_sha256: String| StorageEvent {
            schema_version: EVENT_SCHEMA_VERSION,
            event_id: event_id.to_owned(),
            document_id: document.document_id.clone(),
            source: DeviceSide::Boox,
            object_path: format!("BOOX_Folder/{}/{pdf_name}", document.document_id),
            source_generation: 1,
            source_revision: 1,
            based_on: RevisionPair::default(),
            content_sha256,
            payload_kind: DevicePayloadKind::DeviceView,
            broker_output: None,
        };

        let missing = event("missing", "b-missing.pdf", "c".repeat(64));
        fs::write(
            outgoing.join("b-missing.pdf.inkbridge.json"),
            serde_json::to_vec_pretty(&missing).unwrap(),
        )
        .unwrap();

        fs::write(outgoing.join("c-corrupt.pdf"), b"truncated").unwrap();
        let corrupt = event(
            "corrupt",
            "c-corrupt.pdf",
            inkbridge_broker::sha256_hex(b"expected complete PDF"),
        );
        fs::write(
            outgoing.join("c-corrupt.pdf.inkbridge.json"),
            serde_json::to_vec_pretty(&corrupt).unwrap(),
        )
        .unwrap();

        let valid_bytes = b"valid NeoReader PDF";
        fs::write(outgoing.join("z-valid.pdf"), valid_bytes).unwrap();
        let valid = event(
            "valid",
            "z-valid.pdf",
            inkbridge_broker::sha256_hex(valid_bytes),
        );
        fs::write(
            outgoing.join("z-valid.pdf.inkbridge.json"),
            serde_json::to_vec_pretty(&valid).unwrap(),
        )
        .unwrap();

        let artifacts = endpoint.finalized_artifacts(&document).unwrap();
        assert_eq!(artifacts.len(), 2);
        assert_eq!(artifacts[0].event, corrupt);
        assert_eq!(artifacts[0].payload_path, outgoing.join("c-corrupt.pdf"));
        assert_eq!(artifacts[1].event, valid);
        assert_eq!(artifacts[1].payload_path, outgoing.join("z-valid.pdf"));
    }
}
