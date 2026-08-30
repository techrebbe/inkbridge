use crate::{
    BrokerDriveOutput, CanonicalFrontier, DeliveredDriveOutput, DeviceArtifactBindingApproval,
    DeviceArtifactBindingDecision, DriveChange, DriveGatewayCheckpoint, DriveGatewayConfig,
    DriveInputDecision, DriveOutputDecision, OriginalRegistrationApproval, PendingDriveInput,
    PreparedDeviceArtifactBinding, PreparedDriveInput, PreparedDriveOutput,
    PreparedOriginalRegistration, RegistrationDecision, DRIVE_GATEWAY_PRODUCER,
};
use inkbridge_broker::{
    sha256_hex, stable_document_id, DevicePayloadKind, DeviceSide, RevisionPair, BROKER_PRODUCER,
};
use std::collections::{btree_map::Entry, BTreeMap, BTreeSet};

const GENERATED_BY_PROPERTY: &str = "inkbridgeGeneratedBy";

pub fn drive_event_id(change: &DriveChange, bytes: &[u8]) -> String {
    drive_event_id_from_parts(
        &change.file.file_id,
        change.file.version,
        &sha256_hex(bytes),
    )
}

fn drive_event_id_from_parts(file_id: &str, version: u64, content_sha256: &str) -> String {
    let seed = format!("{}\0{}\0{}", file_id, version, content_sha256);
    format!("drive-v1-{}", sha256_hex(seed.as_bytes()))
}

pub fn prepare_drive_input(
    config: &DriveGatewayConfig,
    checkpoint: &DriveGatewayCheckpoint,
    change: &DriveChange,
    bytes: &[u8],
    _frontier: CanonicalFrontier,
) -> Result<DriveInputDecision, String> {
    config.validate()?;
    checkpoint.validate()?;
    if change.file.trashed {
        return Ok(DriveInputDecision::Ignore {
            reason:
                "Drive file is trashed; annotation deletion must arrive as a tombstone manifest"
                    .to_owned(),
        });
    }
    let Some(binding) = checkpoint.binding_for_file(&change.file.file_id) else {
        if change
            .file
            .app_properties
            .get(GENERATED_BY_PROPERTY)
            .is_some_and(|producer| {
                producer == BROKER_PRODUCER || producer == DRIVE_GATEWAY_PRODUCER
            })
        {
            return Ok(DriveInputDecision::Ignore {
                reason: "uncommitted InkBridge-generated Drive file is not a device input"
                    .to_owned(),
            });
        }
        return Ok(DriveInputDecision::Unbound {
            file_id: change.file.file_id.clone(),
        });
    };
    if change.file.size != bytes.len() as u64 {
        return Err(format!(
            "Drive download length {} does not match declared size {}",
            bytes.len(),
            change.file.size
        ));
    }
    let content_sha256 = sha256_hex(bytes);
    let event_id =
        drive_event_id_from_parts(&change.file.file_id, change.file.version, &content_sha256);
    if checkpoint
        .accepted_file_content_sha256
        .get(&change.file.file_id)
        == Some(&content_sha256)
    {
        return Ok(DriveInputDecision::Duplicate {
            drive_event_id: event_id,
        });
    }
    if checkpoint.processed_drive_events.contains(&event_id) {
        return Ok(DriveInputDecision::Duplicate {
            drive_event_id: event_id,
        });
    }
    if let Some(pending) = checkpoint
        .pending_drive_inputs
        .values()
        .find(|pending| pending.drive_file_id == change.file.file_id)
    {
        return Ok(DriveInputDecision::Deferred {
            file_id: change.file.file_id.clone(),
            pending_drive_event_id: pending.drive_event_id.clone(),
        });
    }
    let source = binding
        .side_for_file(&change.file.file_id)
        .ok_or_else(|| "ambiguous Drive file binding".to_owned())?;
    if !change
        .file
        .parents
        .iter()
        .any(|parent| parent == config.folder_id(source))
    {
        return Ok(DriveInputDecision::Ignore {
            reason: "bound Drive file is outside its configured device folder".to_owned(),
        });
    }
    let Some(payload_kind) = supported_payload_kind(source, &change.file.mime_type) else {
        return Ok(DriveInputDecision::Ignore {
            reason: format!(
                "unsupported {:?} Drive MIME type {}",
                source, change.file.mime_type
            ),
        });
    };
    let based_on = checkpoint
        .file_observed_frontiers
        .get(&change.file.file_id)
        .copied()
        .ok_or_else(|| "bound Drive file lacks an observed revision frontier".to_owned())?;
    let source_revision = based_on
        .get(source)
        .checked_add(1)
        .ok_or_else(|| "source revision overflow".to_owned())?;
    let side_folder = match source {
        DeviceSide::Boox => "BOOX_Folder",
        DeviceSide::Supernote => "Supernote_Folder",
    };
    let extension = match payload_kind {
        DevicePayloadKind::DeviceView if source == DeviceSide::Boox => "pdf",
        _ => "json",
    };
    let object_key = sha256_hex(event_id.as_bytes());
    let gcs_object_path = format!(
        "{side_folder}/{}/drive/{object_key}.{extension}",
        binding.document_id
    );
    let metadata = BTreeMap::from([
        (
            "inkbridge-document-id".to_owned(),
            binding.document_id.clone(),
        ),
        (
            "inkbridge-source-revision".to_owned(),
            source_revision.to_string(),
        ),
        (
            "inkbridge-based-on-boox".to_owned(),
            based_on.boox.to_string(),
        ),
        (
            "inkbridge-based-on-supernote".to_owned(),
            based_on.supernote.to_string(),
        ),
        ("inkbridge-sync-ready".to_owned(), "true".to_owned()),
        (
            "inkbridge-payload-kind".to_owned(),
            match payload_kind {
                DevicePayloadKind::DeviceView => "device_view",
                DevicePayloadKind::BooxOperationManifest => "boox_operation_manifest",
            }
            .to_owned(),
        ),
        (
            "inkbridge-content-sha256".to_owned(),
            content_sha256.clone(),
        ),
        (
            "inkbridge-drive-file-id".to_owned(),
            change.file.file_id.clone(),
        ),
        (
            "inkbridge-drive-file-version".to_owned(),
            change.file.version.to_string(),
        ),
        ("inkbridge-drive-event-id".to_owned(), event_id.clone()),
    ]);
    Ok(DriveInputDecision::Upload(PreparedDriveInput {
        drive_event_id: event_id,
        drive_file_id: change.file.file_id.clone(),
        gcs_object_path,
        content_sha256,
        metadata,
        document_id: binding.document_id.clone(),
        source,
        source_revision,
        based_on,
        payload_kind,
    }))
}

fn supported_payload_kind(source: DeviceSide, mime_type: &str) -> Option<DevicePayloadKind> {
    match source {
        DeviceSide::Boox if mime_type == "application/pdf" => Some(DevicePayloadKind::DeviceView),
        DeviceSide::Boox if mime_type == "application/json" => {
            Some(DevicePayloadKind::BooxOperationManifest)
        }
        DeviceSide::Supernote if mime_type == "application/json" => {
            Some(DevicePayloadKind::DeviceView)
        }
        _ => None,
    }
}

pub fn commit_drive_input(
    checkpoint: &mut DriveGatewayCheckpoint,
    input: &PreparedDriveInput,
) -> Result<(), String> {
    let binding = checkpoint
        .binding_for_file(&input.drive_file_id)
        .ok_or_else(|| format!("unbound Drive input file {}", input.drive_file_id))?;
    if binding.document_id != input.document_id
        || binding.side_for_file(&input.drive_file_id) != Some(input.source)
    {
        return Err(format!(
            "Drive input file {} no longer matches its prepared binding",
            input.drive_file_id
        ));
    }
    if input.source_revision
        != input
            .based_on
            .get(input.source)
            .checked_add(1)
            .ok_or_else(|| "source revision overflow".to_owned())?
    {
        return Err("prepared Drive input has an invalid source revision".to_owned());
    }
    let current_file_frontier = checkpoint
        .file_observed_frontiers
        .get(&input.drive_file_id)
        .copied()
        .ok_or_else(|| "Drive input file lost its observed frontier".to_owned())?;
    if current_file_frontier != input.based_on {
        return Err("prepared Drive input is stale against its file frontier".to_owned());
    }
    let mut proposed_frontier = input.based_on;
    proposed_frontier.set(input.source, input.source_revision);
    let pending = PendingDriveInput {
        drive_event_id: input.drive_event_id.clone(),
        drive_file_id: input.drive_file_id.clone(),
        document_id: input.document_id.clone(),
        source: input.source,
        content_sha256: input.content_sha256.clone(),
        previous_frontier: input.based_on,
        proposed_frontier,
    };
    if let Some(existing) = checkpoint.pending_drive_inputs.get(&input.drive_event_id) {
        if existing == &pending {
            return Ok(());
        }
        return Err(format!(
            "pending Drive event {} has different content",
            input.drive_event_id
        ));
    }
    if checkpoint
        .pending_drive_inputs
        .values()
        .any(|existing| existing.drive_file_id == input.drive_file_id)
    {
        return Err(format!(
            "Drive file {} already has a pending input",
            input.drive_file_id
        ));
    }
    if checkpoint
        .processed_drive_events
        .contains(&input.drive_event_id)
    {
        if checkpoint
            .accepted_file_content_sha256
            .get(&input.drive_file_id)
            == Some(&input.content_sha256)
        {
            return Ok(());
        }
        return Err(format!(
            "Drive event {} was already finalized with different content",
            input.drive_event_id
        ));
    }
    checkpoint
        .processed_drive_events
        .insert(input.drive_event_id.clone());
    checkpoint
        .pending_drive_inputs
        .insert(input.drive_event_id.clone(), pending);
    checkpoint.validate()
}

pub fn accept_drive_input(
    checkpoint: &mut DriveGatewayCheckpoint,
    drive_event_id: &str,
) -> Result<(), String> {
    checkpoint.validate()?;
    let Some(pending) = checkpoint.pending_drive_inputs.remove(drive_event_id) else {
        return Ok(());
    };
    if checkpoint
        .file_observed_frontiers
        .get(&pending.drive_file_id)
        != Some(&pending.previous_frontier)
    {
        checkpoint
            .pending_drive_inputs
            .insert(pending.drive_event_id.clone(), pending);
        return Err("pending Drive input is stale against its file frontier".to_owned());
    }
    checkpoint
        .file_observed_frontiers
        .insert(pending.drive_file_id.clone(), pending.proposed_frontier);
    checkpoint
        .accepted_file_content_sha256
        .insert(pending.drive_file_id, pending.content_sha256);
    checkpoint.validate()
}

pub fn reject_drive_input(
    checkpoint: &mut DriveGatewayCheckpoint,
    drive_event_id: &str,
) -> Result<(), String> {
    checkpoint.validate()?;
    checkpoint.pending_drive_inputs.remove(drive_event_id);
    checkpoint.validate()
}

pub fn prepare_original_registration(
    config: &DriveGatewayConfig,
    checkpoint: &DriveGatewayCheckpoint,
    change: &DriveChange,
    bytes: &[u8],
    approval: &OriginalRegistrationApproval,
) -> Result<RegistrationDecision, String> {
    config.validate()?;
    checkpoint.validate()?;
    if change.file.trashed || change.file.mime_type != "application/pdf" {
        return Ok(RegistrationDecision::Ignore {
            reason: "only a live PDF can register an original document".to_owned(),
        });
    }
    if change
        .file
        .app_properties
        .get(GENERATED_BY_PROPERTY)
        .is_some_and(|producer| producer == BROKER_PRODUCER || producer == DRIVE_GATEWAY_PRODUCER)
    {
        return Ok(RegistrationDecision::Ignore {
            reason: "InkBridge-generated PDF cannot become an immutable original".to_owned(),
        });
    }
    if change.file.size != bytes.len() as u64 {
        return Err(format!(
            "Drive download length {} does not match declared size {}",
            bytes.len(),
            change.file.size
        ));
    }
    let source = match (
        change.file.parents.contains(&config.boox_folder_id),
        change.file.parents.contains(&config.supernote_folder_id),
    ) {
        (true, false) => DeviceSide::Boox,
        (false, true) => DeviceSide::Supernote,
        _ => {
            return Ok(RegistrationDecision::Ignore {
                reason: "unbound original must be directly inside exactly one device folder"
                    .to_owned(),
            });
        }
    };
    let actual_hash = sha256_hex(bytes);
    if approval.drive_file_id != change.file.file_id
        || approval.drive_file_version != change.file.version
        || approval.content_sha256 != actual_hash
    {
        return Ok(RegistrationDecision::Ignore {
            reason: "unbound PDF lacks approval for this exact clean original revision".to_owned(),
        });
    }
    let event_id = drive_event_id(change, bytes);
    if checkpoint.processed_drive_events.contains(&event_id) {
        return Ok(RegistrationDecision::Duplicate {
            drive_event_id: event_id,
        });
    }
    let original_pdf_sha256 = actual_hash;
    let document_id = stable_document_id(bytes);
    let staging_key = sha256_hex(event_id.as_bytes());
    let gcs_object_path = format!("Staging/drive-{staging_key}.pdf");
    let metadata = BTreeMap::from([
        ("inkbridge-register-original".to_owned(), "true".to_owned()),
        (
            "inkbridge-original-file-name".to_owned(),
            change.file.name.clone(),
        ),
        ("inkbridge-document-id".to_owned(), document_id.clone()),
        (
            "inkbridge-content-sha256".to_owned(),
            original_pdf_sha256.clone(),
        ),
        (
            "inkbridge-drive-file-id".to_owned(),
            change.file.file_id.clone(),
        ),
        (
            "inkbridge-drive-file-version".to_owned(),
            change.file.version.to_string(),
        ),
        ("inkbridge-drive-event-id".to_owned(), event_id.clone()),
    ]);
    Ok(RegistrationDecision::Register(
        PreparedOriginalRegistration {
            drive_event_id: event_id,
            drive_file_id: change.file.file_id.clone(),
            source,
            document_id,
            original_pdf_sha256,
            gcs_object_path,
            metadata,
        },
    ))
}

pub fn commit_original_registration(
    checkpoint: &mut DriveGatewayCheckpoint,
    registration: &PreparedOriginalRegistration,
) -> Result<(), String> {
    checkpoint.validate()?;
    if let Some(existing) = checkpoint.binding_for_file(&registration.drive_file_id) {
        if existing.document_id != registration.document_id {
            return Err(format!(
                "Drive file {} is already bound to {}",
                registration.drive_file_id, existing.document_id
            ));
        }
        if existing.side_for_file(&registration.drive_file_id) != Some(registration.source) {
            return Err(format!(
                "Drive file {} is already bound to the other device side",
                registration.drive_file_id
            ));
        }
    }
    if checkpoint
        .documents
        .get(&registration.document_id)
        .is_some_and(|binding| binding.original_pdf_sha256 != registration.original_pdf_sha256)
    {
        return Err("registration collides with a different original PDF".to_owned());
    }
    if checkpoint
        .processed_drive_events
        .contains(&registration.drive_event_id)
    {
        if checkpoint
            .binding_for_file(&registration.drive_file_id)
            .is_some_and(|binding| {
                binding.document_id == registration.document_id
                    && binding.side_for_file(&registration.drive_file_id)
                        == Some(registration.source)
            })
        {
            return Ok(());
        }
        return Err("processed original registration lacks its file binding".to_owned());
    }
    checkpoint
        .processed_drive_events
        .insert(registration.drive_event_id.clone());
    checkpoint.accepted_file_content_sha256.insert(
        registration.drive_file_id.clone(),
        registration.original_pdf_sha256.clone(),
    );
    checkpoint
        .file_observed_frontiers
        .insert(registration.drive_file_id.clone(), RevisionPair::default());
    match checkpoint.documents.entry(registration.document_id.clone()) {
        Entry::Vacant(entry) => {
            let mut boox_file_ids = BTreeSet::new();
            let mut supernote_file_ids = BTreeSet::new();
            match registration.source {
                DeviceSide::Boox => boox_file_ids.insert(registration.drive_file_id.clone()),
                DeviceSide::Supernote => {
                    supernote_file_ids.insert(registration.drive_file_id.clone())
                }
            };
            entry.insert(crate::DocumentBinding {
                document_id: registration.document_id.clone(),
                original_pdf_sha256: registration.original_pdf_sha256.clone(),
                boox_file_ids,
                supernote_file_ids,
            });
        }
        Entry::Occupied(mut entry) => {
            let binding = entry.get_mut();
            match registration.source {
                DeviceSide::Boox => binding
                    .boox_file_ids
                    .insert(registration.drive_file_id.clone()),
                DeviceSide::Supernote => binding
                    .supernote_file_ids
                    .insert(registration.drive_file_id.clone()),
            };
        }
    }
    checkpoint.validate()
}

pub fn prepare_device_artifact_binding(
    config: &DriveGatewayConfig,
    checkpoint: &DriveGatewayCheckpoint,
    change: &DriveChange,
    bytes: &[u8],
    approval: &DeviceArtifactBindingApproval,
) -> Result<DeviceArtifactBindingDecision, String> {
    config.validate()?;
    checkpoint.validate()?;
    if change.file.trashed {
        return Ok(DeviceArtifactBindingDecision::Ignore {
            reason: "trashed Drive artifact cannot be bound".to_owned(),
        });
    }
    if let Some(existing) = checkpoint.binding_for_file(&change.file.file_id) {
        if existing.document_id == approval.document_id
            && existing.side_for_file(&change.file.file_id) == Some(approval.source)
        {
            return Ok(DeviceArtifactBindingDecision::AlreadyBound {
                file_id: change.file.file_id.clone(),
            });
        }
        return Err(format!(
            "Drive file {} is already bound elsewhere",
            change.file.file_id
        ));
    }
    if !checkpoint.documents.contains_key(&approval.document_id) {
        return Ok(DeviceArtifactBindingDecision::Ignore {
            reason: format!("unknown InkBridge document {}", approval.document_id),
        });
    }
    if change
        .file
        .app_properties
        .get(GENERATED_BY_PROPERTY)
        .is_some_and(|producer| producer == BROKER_PRODUCER || producer == DRIVE_GATEWAY_PRODUCER)
    {
        return Ok(DeviceArtifactBindingDecision::Ignore {
            reason:
                "uncommitted InkBridge-generated file cannot be registered as a device artifact"
                    .to_owned(),
        });
    }
    if !change
        .file
        .parents
        .iter()
        .any(|parent| parent == config.folder_id(approval.source))
        || change
            .file
            .parents
            .iter()
            .any(|parent| parent == config.folder_id(approval.source.other()))
    {
        return Ok(DeviceArtifactBindingDecision::Ignore {
            reason: "device artifact is not directly inside only its approved device folder"
                .to_owned(),
        });
    }
    if supported_payload_kind(approval.source, &change.file.mime_type).is_none() {
        return Ok(DeviceArtifactBindingDecision::Ignore {
            reason: format!(
                "unsupported {:?} Drive MIME type {}",
                approval.source, change.file.mime_type
            ),
        });
    }
    if change.file.size != bytes.len() as u64 {
        return Err(format!(
            "Drive download length {} does not match declared size {}",
            bytes.len(),
            change.file.size
        ));
    }
    let content_sha256 = sha256_hex(bytes);
    if change.file.version == 0
        || approval.drive_file_id != change.file.file_id
        || approval.drive_file_version != change.file.version
        || approval.content_sha256 != content_sha256
    {
        return Ok(DeviceArtifactBindingDecision::Ignore {
            reason: "device artifact lacks approval for this exact Drive revision".to_owned(),
        });
    }
    Ok(DeviceArtifactBindingDecision::Bind(
        PreparedDeviceArtifactBinding {
            drive_file_id: change.file.file_id.clone(),
            drive_file_version: change.file.version,
            content_sha256,
            document_id: approval.document_id.clone(),
            source: approval.source,
            based_on: approval.based_on,
        },
    ))
}

pub fn commit_device_artifact_binding(
    checkpoint: &mut DriveGatewayCheckpoint,
    artifact: &PreparedDeviceArtifactBinding,
) -> Result<(), String> {
    checkpoint.validate()?;
    if let Some(existing) = checkpoint.binding_for_file(&artifact.drive_file_id) {
        if existing.document_id == artifact.document_id
            && existing.side_for_file(&artifact.drive_file_id) == Some(artifact.source)
        {
            return Ok(());
        }
        return Err(format!(
            "Drive file {} is already bound elsewhere",
            artifact.drive_file_id
        ));
    }
    let binding = checkpoint
        .documents
        .get_mut(&artifact.document_id)
        .ok_or_else(|| format!("unknown InkBridge document {}", artifact.document_id))?;
    match artifact.source {
        DeviceSide::Boox => binding.boox_file_ids.insert(artifact.drive_file_id.clone()),
        DeviceSide::Supernote => binding
            .supernote_file_ids
            .insert(artifact.drive_file_id.clone()),
    };
    checkpoint
        .file_observed_frontiers
        .insert(artifact.drive_file_id.clone(), artifact.based_on);
    checkpoint.validate()
}

pub fn prepare_drive_output(
    config: &DriveGatewayConfig,
    checkpoint: &DriveGatewayCheckpoint,
    output: &BrokerDriveOutput,
) -> Result<DriveOutputDecision, String> {
    config.validate()?;
    checkpoint.validate()?;
    if !checkpoint.documents.contains_key(&output.document_id) {
        return Err(format!(
            "broker output references unbound document {}",
            output.document_id
        ));
    }
    if output.gcs_generation == 0
        || output.content_sha256.len() != 64
        || output.event_id.trim().is_empty()
    {
        return Err("broker output marker is incomplete".to_owned());
    }
    let delivery_seed = format!(
        "{}\0{}\0{}\0{}\0{}",
        output.gcs_object_path,
        output.gcs_generation,
        output.document_id,
        output.event_id,
        output.content_sha256
    );
    let delivery_id = format!("drive-delivery-v1-{}", sha256_hex(delivery_seed.as_bytes()));
    if checkpoint
        .delivered_broker_outputs
        .contains_key(&delivery_id)
    {
        return Ok(DriveOutputDecision::Duplicate { delivery_id });
    }
    let extension = safe_extension(&output.file_extension)?;
    let side_name = match output.target {
        DeviceSide::Boox => "boox",
        DeviceSide::Supernote => "supernote",
    };
    let event_hash = sha256_hex(output.event_id.as_bytes());
    let file_name = format!(
        "inkbridge-{side_name}-r{}-r{}-{}.{}",
        output.source_revisions.boox,
        output.source_revisions.supernote,
        &event_hash[..16],
        extension
    );
    let app_properties = BTreeMap::from([
        (GENERATED_BY_PROPERTY.to_owned(), BROKER_PRODUCER.to_owned()),
        ("inkbridgeEventId".to_owned(), output.event_id.clone()),
        ("inkbridgeDocumentId".to_owned(), output.document_id.clone()),
        (
            "inkbridgeSourceRevisions".to_owned(),
            format!(
                "{}:{}",
                output.source_revisions.boox, output.source_revisions.supernote
            ),
        ),
        (
            "inkbridgeContentSha256".to_owned(),
            output.content_sha256.clone(),
        ),
        (
            "inkbridgeGcsGeneration".to_owned(),
            output.gcs_generation.to_string(),
        ),
        ("inkbridgeDeliveryId".to_owned(), delivery_id.clone()),
    ]);
    Ok(DriveOutputDecision::Create(PreparedDriveOutput {
        delivery_id,
        document_id: output.document_id.clone(),
        target: output.target,
        content_sha256: output.content_sha256.clone(),
        source_revisions: output.source_revisions,
        parent_folder_id: config.folder_id(output.target).to_owned(),
        file_name,
        app_properties,
    }))
}

fn safe_extension(value: &str) -> Result<&str, String> {
    let value = value.trim_start_matches('.');
    if matches!(value, "pdf" | "json") {
        Ok(value)
    } else {
        Err(format!("unsupported Drive output extension {value}"))
    }
}

pub fn commit_drive_output(
    checkpoint: &mut DriveGatewayCheckpoint,
    output: &PreparedDriveOutput,
    drive_file_id: String,
    drive_file_version: u64,
) -> Result<(), String> {
    checkpoint.validate()?;
    if drive_file_id.trim().is_empty() || drive_file_version == 0 {
        return Err("created Drive output identity is incomplete".to_owned());
    }
    let delivered = DeliveredDriveOutput {
        delivery_id: output.delivery_id.clone(),
        drive_file_id: drive_file_id.clone(),
        drive_file_version,
        document_id: output.document_id.clone(),
        target: output.target,
        content_sha256: output.content_sha256.clone(),
        source_revisions: output.source_revisions,
    };
    if let Some(existing) = checkpoint.delivered_broker_outputs.get(&output.delivery_id) {
        if existing == &delivered {
            return Ok(());
        }
        return Err(format!(
            "Drive delivery {} was already committed to another file revision",
            output.delivery_id
        ));
    }
    if let Some(existing) = checkpoint.binding_for_file(&drive_file_id) {
        if existing.document_id != output.document_id
            || existing.side_for_file(&drive_file_id) != Some(output.target)
        {
            return Err(format!(
                "created Drive file {drive_file_id} is already bound elsewhere"
            ));
        }
    }
    let binding = checkpoint
        .documents
        .get_mut(&output.document_id)
        .ok_or_else(|| format!("unbound output document {}", output.document_id))?;
    match output.target {
        DeviceSide::Boox => binding.boox_file_ids.insert(drive_file_id.clone()),
        DeviceSide::Supernote => binding.supernote_file_ids.insert(drive_file_id.clone()),
    };
    checkpoint
        .processed_drive_events
        .insert(drive_event_id_from_parts(
            &drive_file_id,
            drive_file_version,
            &output.content_sha256,
        ));
    checkpoint
        .accepted_file_content_sha256
        .insert(drive_file_id.clone(), output.content_sha256.clone());
    checkpoint
        .file_observed_frontiers
        .insert(drive_file_id.clone(), output.source_revisions);
    checkpoint
        .delivered_broker_outputs
        .insert(output.delivery_id.clone(), delivered);
    checkpoint.validate()
}

pub fn commit_page_token(checkpoint: &mut DriveGatewayCheckpoint, new_start_page_token: String) {
    checkpoint.next_page_token = Some(new_start_page_token);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BrokerDriveOutput, DocumentBinding, DriveFileRevision, DriveGatewayConfig,
        DRIVE_GATEWAY_SCHEMA_VERSION,
    };
    use inkbridge_broker::RevisionPair;
    use std::collections::{BTreeMap, BTreeSet};

    const DOC_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn document_id() -> String {
        format!("inkbridge-doc-v1-{DOC_HASH}")
    }

    fn checkpoint() -> DriveGatewayCheckpoint {
        DriveGatewayCheckpoint {
            schema_version: DRIVE_GATEWAY_SCHEMA_VERSION,
            next_page_token: Some("100".to_owned()),
            documents: BTreeMap::from([(
                document_id(),
                DocumentBinding {
                    document_id: document_id(),
                    original_pdf_sha256: DOC_HASH.to_owned(),
                    boox_file_ids: BTreeSet::from(["boox-file".to_owned()]),
                    supernote_file_ids: BTreeSet::from(["supernote-file".to_owned()]),
                },
            )]),
            processed_drive_events: BTreeSet::new(),
            accepted_file_content_sha256: BTreeMap::new(),
            file_observed_frontiers: BTreeMap::from([
                (
                    "boox-file".to_owned(),
                    RevisionPair {
                        boox: 3,
                        supernote: 5,
                    },
                ),
                (
                    "supernote-file".to_owned(),
                    RevisionPair {
                        boox: 2,
                        supernote: 8,
                    },
                ),
            ]),
            pending_drive_inputs: BTreeMap::new(),
            delivered_broker_outputs: BTreeMap::new(),
        }
    }

    fn config() -> DriveGatewayConfig {
        DriveGatewayConfig {
            schema_version: DRIVE_GATEWAY_SCHEMA_VERSION,
            boox_folder_id: "boox-folder-id-123".to_owned(),
            supernote_folder_id: "supernote-folder-id-123".to_owned(),
        }
    }

    fn change(file_id: &str, mime_type: &str) -> DriveChange {
        DriveChange {
            file: DriveFileRevision {
                file_id: file_id.to_owned(),
                name: "renamed-anything.pdf".to_owned(),
                version: 17,
                mime_type: mime_type.to_owned(),
                parents: vec![if file_id.starts_with("supernote") {
                    config().supernote_folder_id
                } else {
                    config().boox_folder_id
                }],
                size: if mime_type == "application/pdf" {
                    7
                } else {
                    18
                },
                trashed: false,
                app_properties: BTreeMap::new(),
            },
        }
    }

    #[test]
    fn mapped_boox_revision_uses_file_identity_not_filename() {
        let bytes = b"pdf ink";
        let decision = prepare_drive_input(
            &config(),
            &checkpoint(),
            &change("boox-file", "application/pdf"),
            bytes,
            CanonicalFrontier {
                revisions: RevisionPair {
                    boox: 3,
                    supernote: 5,
                },
            },
        )
        .unwrap();
        let DriveInputDecision::Upload(input) = decision else {
            panic!("expected upload")
        };
        assert_eq!(input.document_id, document_id());
        assert_eq!(input.source, DeviceSide::Boox);
        assert_eq!(input.source_revision, 4);
        assert_eq!(
            input.based_on,
            RevisionPair {
                boox: 3,
                supernote: 5
            }
        );
        assert!(input
            .gcs_object_path
            .starts_with(&format!("BOOX_Folder/{}/drive/", document_id())));
        assert_eq!(input.metadata["inkbridge-drive-file-id"], "boox-file");
    }

    #[test]
    fn duplicate_drive_delivery_cannot_duplicate_broker_input() {
        let bytes = b"native page export";
        let change = change("supernote-file", "application/json");
        let frontier = CanonicalFrontier {
            revisions: RevisionPair {
                boox: 2,
                supernote: 8,
            },
        };
        let mut checkpoint = checkpoint();
        let DriveInputDecision::Upload(input) =
            prepare_drive_input(&config(), &checkpoint, &change, bytes, frontier.clone()).unwrap()
        else {
            panic!("expected upload")
        };
        commit_drive_input(&mut checkpoint, &input).unwrap();
        assert_eq!(
            prepare_drive_input(&config(), &checkpoint, &change, bytes, frontier).unwrap(),
            DriveInputDecision::Duplicate {
                drive_event_id: input.drive_event_id
            }
        );
    }

    #[test]
    fn uncommitted_generated_drive_file_is_ignored_to_prevent_a_loop() {
        let mut change = change("generated-unbound", "application/pdf");
        change
            .file
            .app_properties
            .insert(GENERATED_BY_PROPERTY.to_owned(), BROKER_PRODUCER.to_owned());
        assert!(matches!(
            prepare_drive_input(
                &config(),
                &checkpoint(),
                &change,
                b"pdf ink",
                CanonicalFrontier {
                    revisions: RevisionPair::default()
                }
            )
            .unwrap(),
            DriveInputDecision::Ignore { .. }
        ));
    }

    #[test]
    fn unbound_file_is_not_guessed_from_matching_name() {
        assert_eq!(
            prepare_drive_input(
                &config(),
                &checkpoint(),
                &change("different-file", "application/pdf"),
                b"view",
                CanonicalFrontier {
                    revisions: RevisionPair::default()
                }
            )
            .unwrap(),
            DriveInputDecision::Unbound {
                file_id: "different-file".to_owned()
            }
        );
    }

    #[test]
    fn approved_device_artifact_can_bind_and_enter_the_broker() {
        let config = config();
        let bytes = b"native page export";
        let mut checkpoint = checkpoint();
        let artifact_change = change("supernote-native-export", "application/json");
        assert!(matches!(
            prepare_drive_input(
                &config,
                &checkpoint,
                &artifact_change,
                bytes,
                CanonicalFrontier {
                    revisions: RevisionPair::default()
                }
            )
            .unwrap(),
            DriveInputDecision::Unbound { .. }
        ));
        let approval = DeviceArtifactBindingApproval {
            drive_file_id: artifact_change.file.file_id.clone(),
            drive_file_version: artifact_change.file.version,
            content_sha256: sha256_hex(bytes),
            document_id: document_id(),
            source: DeviceSide::Supernote,
            based_on: RevisionPair::default(),
        };
        let DeviceArtifactBindingDecision::Bind(prepared) = prepare_device_artifact_binding(
            &config,
            &checkpoint,
            &artifact_change,
            bytes,
            &approval,
        )
        .unwrap() else {
            panic!("expected explicit device artifact binding")
        };
        commit_device_artifact_binding(&mut checkpoint, &prepared).unwrap();
        let DriveInputDecision::Upload(input) = prepare_drive_input(
            &config,
            &checkpoint,
            &artifact_change,
            bytes,
            CanonicalFrontier {
                revisions: RevisionPair::default(),
            },
        )
        .unwrap() else {
            panic!("expected newly bound artifact to enter the broker")
        };
        assert_eq!(input.document_id, document_id());
        assert_eq!(input.source, DeviceSide::Supernote);
        assert_eq!(input.drive_file_id, "supernote-native-export");
    }

    #[test]
    fn metadata_only_drive_version_does_not_create_a_source_revision() {
        let config = config();
        let bytes = b"pdf ink";
        let mut checkpoint = checkpoint();
        let original_change = change("boox-file", "application/pdf");
        let frontier = CanonicalFrontier {
            revisions: RevisionPair {
                boox: 4,
                supernote: 2,
            },
        };
        checkpoint
            .file_observed_frontiers
            .insert("boox-file".to_owned(), frontier.revisions);
        let DriveInputDecision::Upload(input) = prepare_drive_input(
            &config,
            &checkpoint,
            &original_change,
            bytes,
            frontier.clone(),
        )
        .unwrap() else {
            panic!("expected initial content revision")
        };
        commit_drive_input(&mut checkpoint, &input).unwrap();
        accept_drive_input(&mut checkpoint, &input.drive_event_id).unwrap();

        let mut renamed = original_change;
        renamed.file.name = "metadata-only-rename.pdf".to_owned();
        renamed.file.version += 1;
        assert!(matches!(
            prepare_drive_input(&config, &checkpoint, &renamed, bytes, frontier).unwrap(),
            DriveInputDecision::Duplicate { .. }
        ));
    }

    #[test]
    fn pending_upload_defers_later_file_versions_until_acceptance() {
        let config = config();
        let mut checkpoint = checkpoint();
        let frontier = CanonicalFrontier {
            revisions: RevisionPair {
                boox: 4,
                supernote: 2,
            },
        };
        checkpoint
            .file_observed_frontiers
            .insert("boox-file".to_owned(), frontier.revisions);
        let first_change = change("boox-file", "application/pdf");
        let DriveInputDecision::Upload(first) = prepare_drive_input(
            &config,
            &checkpoint,
            &first_change,
            b"pdf ink",
            frontier.clone(),
        )
        .unwrap() else {
            panic!("expected first upload")
        };
        assert_eq!(first.source_revision, 5);
        commit_drive_input(&mut checkpoint, &first).unwrap();
        assert_eq!(checkpoint.file_observed_frontiers["boox-file"].boox, 4);

        let second_bytes = b"pdf ink plus another stroke";
        let mut second_change = first_change;
        second_change.file.version += 1;
        second_change.file.size = second_bytes.len() as u64;
        assert!(matches!(
            prepare_drive_input(
                &config,
                &checkpoint,
                &second_change,
                second_bytes,
                frontier.clone()
            )
            .unwrap(),
            DriveInputDecision::Deferred { .. }
        ));
        accept_drive_input(&mut checkpoint, &first.drive_event_id).unwrap();
        assert_eq!(checkpoint.file_observed_frontiers["boox-file"].boox, 5);

        let DriveInputDecision::Upload(second) =
            prepare_drive_input(&config, &checkpoint, &second_change, second_bytes, frontier)
                .unwrap()
        else {
            panic!("expected second upload")
        };
        assert_eq!(second.based_on.boox, 5);
        assert_eq!(second.based_on.supernote, 2);
        assert_eq!(second.source_revision, 6);
        commit_drive_input(&mut checkpoint, &second).unwrap();
        assert_eq!(checkpoint.file_observed_frontiers["boox-file"].boox, 5);
        accept_drive_input(&mut checkpoint, &second.drive_event_id).unwrap();
        assert_eq!(checkpoint.file_observed_frontiers["boox-file"].boox, 6);
    }

    #[test]
    fn pending_opposite_side_upload_does_not_hide_concurrency() {
        let config = config();
        let mut checkpoint = checkpoint();
        let frontier = CanonicalFrontier {
            revisions: RevisionPair {
                boox: 4,
                supernote: 2,
            },
        };
        checkpoint
            .file_observed_frontiers
            .insert("boox-file".to_owned(), frontier.revisions);
        checkpoint
            .file_observed_frontiers
            .insert("supernote-file".to_owned(), frontier.revisions);
        let DriveInputDecision::Upload(boox) = prepare_drive_input(
            &config,
            &checkpoint,
            &change("boox-file", "application/pdf"),
            b"pdf ink",
            frontier.clone(),
        )
        .unwrap() else {
            panic!("expected BOOX upload")
        };
        commit_drive_input(&mut checkpoint, &boox).unwrap();

        let DriveInputDecision::Upload(supernote) = prepare_drive_input(
            &config,
            &checkpoint,
            &change("supernote-file", "application/json"),
            b"native page export",
            frontier,
        )
        .unwrap() else {
            panic!("expected concurrent Supernote upload")
        };
        assert_eq!(supernote.based_on.boox, 4);
        assert_eq!(supernote.based_on.supernote, 2);
        assert_eq!(supernote.source_revision, 3);
    }

    #[test]
    fn rejected_upload_keeps_the_previous_file_frontier() {
        let config = config();
        let mut checkpoint = checkpoint();
        let base = RevisionPair {
            boox: 4,
            supernote: 2,
        };
        checkpoint
            .file_observed_frontiers
            .insert("boox-file".to_owned(), base);
        let first_bytes = b"malformed pdf ink";
        let mut first_change = change("boox-file", "application/pdf");
        first_change.file.size = first_bytes.len() as u64;
        let DriveInputDecision::Upload(first) = prepare_drive_input(
            &config,
            &checkpoint,
            &first_change,
            first_bytes,
            CanonicalFrontier { revisions: base },
        )
        .unwrap() else {
            panic!("expected first upload")
        };
        commit_drive_input(&mut checkpoint, &first).unwrap();
        reject_drive_input(&mut checkpoint, &first.drive_event_id).unwrap();
        assert_eq!(checkpoint.file_observed_frontiers["boox-file"], base);

        let corrected_bytes = b"corrected pdf ink";
        let mut corrected_change = first_change;
        corrected_change.file.version += 1;
        corrected_change.file.size = corrected_bytes.len() as u64;
        let DriveInputDecision::Upload(corrected) = prepare_drive_input(
            &config,
            &checkpoint,
            &corrected_change,
            corrected_bytes,
            CanonicalFrontier { revisions: base },
        )
        .unwrap() else {
            panic!("expected corrected upload")
        };
        assert_eq!(corrected.based_on, base);
        assert_eq!(corrected.source_revision, 5);
    }

    #[test]
    fn broker_outputs_are_create_only_and_idempotent() {
        let config = config();
        let generated_bytes = b"generated view";
        let output = BrokerDriveOutput {
            gcs_object_path: "BOOX_Folder/doc/view.pdf".to_owned(),
            gcs_generation: 29,
            document_id: document_id(),
            target: DeviceSide::Boox,
            event_id: "broker-event".to_owned(),
            source_revisions: RevisionPair {
                boox: 4,
                supernote: 9,
            },
            content_sha256: sha256_hex(generated_bytes),
            file_extension: "pdf".to_owned(),
        };
        let mut checkpoint = checkpoint();
        let DriveOutputDecision::Create(plan) =
            prepare_drive_output(&config, &checkpoint, &output).unwrap()
        else {
            panic!("expected create")
        };
        assert_eq!(plan.parent_folder_id, config.boox_folder_id);
        assert_eq!(plan.app_properties[GENERATED_BY_PROPERTY], BROKER_PRODUCER);
        commit_drive_output(&mut checkpoint, &plan, "boox-created-file".to_owned(), 30).unwrap();
        commit_drive_output(&mut checkpoint, &plan, "boox-created-file".to_owned(), 30).unwrap();
        assert!(checkpoint
            .documents
            .get(&document_id())
            .unwrap()
            .boox_file_ids
            .contains("boox-created-file"));
        assert_eq!(
            prepare_drive_output(&config, &checkpoint, &output).unwrap(),
            DriveOutputDecision::Duplicate {
                delivery_id: plan.delivery_id
            }
        );

        let mut generated_change = change("boox-created-file", "application/pdf");
        generated_change.file.version = 30;
        generated_change.file.size = generated_bytes.len() as u64;
        generated_change.file.app_properties = plan.app_properties.clone();
        assert!(matches!(
            prepare_drive_input(
                &config,
                &checkpoint,
                &generated_change,
                generated_bytes,
                CanonicalFrontier {
                    revisions: output.source_revisions
                }
            )
            .unwrap(),
            DriveInputDecision::Duplicate { .. }
        ));

        let edited_bytes = b"generated view plus user ink";
        generated_change.file.version = 31;
        generated_change.file.size = edited_bytes.len() as u64;
        let DriveInputDecision::Upload(user_edit) = prepare_drive_input(
            &config,
            &checkpoint,
            &generated_change,
            edited_bytes,
            CanonicalFrontier {
                revisions: RevisionPair {
                    boox: 20,
                    supernote: 30,
                },
            },
        )
        .unwrap() else {
            panic!("expected a later edit of the created file to re-enter the broker")
        };
        assert_eq!(user_edit.source, DeviceSide::Boox);
        assert_eq!(user_edit.document_id, document_id());
        assert_eq!(user_edit.based_on, output.source_revisions);
        assert_eq!(user_edit.source_revision, output.source_revisions.boox + 1);
    }

    #[test]
    fn page_token_advances_only_when_caller_commits_it() {
        let mut checkpoint = checkpoint();
        assert_eq!(checkpoint.next_page_token.as_deref(), Some("100"));
        commit_page_token(&mut checkpoint, "101".to_owned());
        assert_eq!(checkpoint.next_page_token.as_deref(), Some("101"));
    }

    #[test]
    fn clean_originals_from_both_folders_join_one_content_identity() {
        let config = config();
        let bytes = b"%PDF-clean";
        let mut checkpoint = DriveGatewayCheckpoint::empty();
        let mut boox = change("boox-new", "application/pdf");
        boox.file.name = "Book.pdf".to_owned();
        boox.file.size = bytes.len() as u64;
        boox.file.parents = vec![config.boox_folder_id.clone()];
        let boox_approval = OriginalRegistrationApproval {
            drive_file_id: boox.file.file_id.clone(),
            drive_file_version: boox.file.version,
            content_sha256: sha256_hex(bytes),
        };
        let RegistrationDecision::Register(boox_registration) =
            prepare_original_registration(&config, &checkpoint, &boox, bytes, &boox_approval)
                .unwrap()
        else {
            panic!("expected BOOX registration")
        };
        commit_original_registration(&mut checkpoint, &boox_registration).unwrap();

        let mut supernote = change("supernote-new", "application/pdf");
        supernote.file.name = "Renamed Book.pdf".to_owned();
        supernote.file.size = bytes.len() as u64;
        supernote.file.parents = vec![config.supernote_folder_id.clone()];
        let supernote_approval = OriginalRegistrationApproval {
            drive_file_id: supernote.file.file_id.clone(),
            drive_file_version: supernote.file.version,
            content_sha256: sha256_hex(bytes),
        };
        let RegistrationDecision::Register(supernote_registration) = prepare_original_registration(
            &config,
            &checkpoint,
            &supernote,
            bytes,
            &supernote_approval,
        )
        .unwrap() else {
            panic!("expected Supernote registration")
        };
        assert_eq!(
            boox_registration.document_id,
            supernote_registration.document_id
        );
        commit_original_registration(&mut checkpoint, &supernote_registration).unwrap();
        let binding = checkpoint
            .documents
            .get(&boox_registration.document_id)
            .unwrap();
        assert!(binding.boox_file_ids.contains("boox-new"));
        assert!(binding.supernote_file_ids.contains("supernote-new"));

        let edited_bytes = b"%PDF-clean-with-ink";
        boox.file.version += 1;
        boox.file.size = edited_bytes.len() as u64;
        let DriveInputDecision::Upload(edit) = prepare_drive_input(
            &config,
            &checkpoint,
            &boox,
            edited_bytes,
            CanonicalFrontier {
                revisions: RevisionPair::default(),
            },
        )
        .unwrap() else {
            panic!("expected first BOOX edit")
        };
        commit_drive_input(&mut checkpoint, &edit).unwrap();
        accept_drive_input(&mut checkpoint, &edit.drive_event_id).unwrap();
        assert_eq!(checkpoint.file_observed_frontiers["boox-new"].boox, 1);

        commit_original_registration(&mut checkpoint, &boox_registration).unwrap();
        assert_eq!(checkpoint.file_observed_frontiers["boox-new"].boox, 1);
    }

    #[test]
    fn bound_file_moved_to_the_other_folder_is_not_ingested() {
        let mut change = change("boox-file", "application/pdf");
        change.file.parents = vec![config().supernote_folder_id];
        assert!(matches!(
            prepare_drive_input(
                &config(),
                &checkpoint(),
                &change,
                b"pdf ink",
                CanonicalFrontier {
                    revisions: RevisionPair::default()
                }
            )
            .unwrap(),
            DriveInputDecision::Ignore { .. }
        ));
    }

    #[test]
    fn registration_rejects_generated_or_unapproved_pdf() {
        let config = config();
        let checkpoint = DriveGatewayCheckpoint::empty();
        let bytes = b"%PDF-clean";
        let mut change = change("boox-new", "application/pdf");
        change.file.size = bytes.len() as u64;
        change.file.parents = vec![config.boox_folder_id.clone()];
        let approval = OriginalRegistrationApproval {
            drive_file_id: change.file.file_id.clone(),
            drive_file_version: change.file.version,
            content_sha256: sha256_hex(bytes),
        };
        change
            .file
            .app_properties
            .insert(GENERATED_BY_PROPERTY.to_owned(), BROKER_PRODUCER.to_owned());
        assert!(matches!(
            prepare_original_registration(&config, &checkpoint, &change, bytes, &approval).unwrap(),
            RegistrationDecision::Ignore { .. }
        ));

        change.file.app_properties.clear();
        let stale_approval = OriginalRegistrationApproval {
            content_sha256: "f".repeat(64),
            ..approval
        };
        assert!(matches!(
            prepare_original_registration(&config, &checkpoint, &change, bytes, &stale_approval)
                .unwrap(),
            RegistrationDecision::Ignore { .. }
        ));
    }
}
