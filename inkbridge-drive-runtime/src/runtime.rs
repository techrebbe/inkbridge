use inkbridge_broker::{
    sha256_hex, CanonicalDocumentState, CommitError, ConditionalWrite, DeviceSide,
    GenerationPrecondition, ProcessOutcome, RevisionPair, StoredObject,
};
use inkbridge_cloud_runtime::{
    CanonicalStateStore, ObjectStore, RegisterDocumentRequest, RuntimeOutcome, RuntimeService,
};
use inkbridge_drive_gateway::{
    accept_drive_input, commit_device_artifact_binding, commit_drive_input, commit_drive_output,
    commit_original_registration, commit_page_token, prepare_device_artifact_binding,
    prepare_drive_input, prepare_drive_output, prepare_original_registration,
    reconcile_drive_output, reject_drive_input, reserve_drive_output, BrokerDriveOutput,
    CanonicalFrontier, DeviceArtifactBindingApproval, DeviceArtifactBindingDecision, DriveChange,
    DriveFileRevision, DriveGatewayCheckpoint, DriveGatewayConfig, DriveInputDecision,
    DriveOutputDecision, OriginalRegistrationApproval, PendingDriveInput, PreparedDriveOutput,
    PreparedOriginalRegistration, RegistrationDecision,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunMode {
    Apply,
    DryRun,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DriveChangePage {
    pub changes: Vec<DriveChange>,
    #[serde(default)]
    pub next_page_token: Option<String>,
    #[serde(default)]
    pub new_start_page_token: Option<String>,
}

impl DriveChangePage {
    fn durable_successor(&self) -> Result<&str, String> {
        match (
            self.next_page_token.as_deref(),
            self.new_start_page_token.as_deref(),
        ) {
            (Some(next), None) if !next.is_empty() => Ok(next),
            (None, Some(start)) if !start.is_empty() => Ok(start),
            _ => Err(
                "Drive change page must contain exactly one nonempty successor token".to_owned(),
            ),
        }
    }
}

pub trait DriveApi: Send + Sync {
    fn start_page_token(&self) -> Result<String, String>;
    fn list_initial_files(&self, folder_ids: &[String]) -> Result<Vec<DriveFileRevision>, String>;
    fn list_changes(&self, page_token: &str) -> Result<DriveChangePage, String>;
    fn download(&self, file_id: &str) -> Result<Vec<u8>, String>;
    fn file_revision(&self, file_id: &str) -> Result<DriveFileRevision, String>;
    fn find_delivery(&self, delivery_id: &str) -> Result<Vec<DriveFileRevision>, String>;
    fn create_delivery(
        &self,
        output: &PreparedDriveOutput,
        bytes: &[u8],
    ) -> Result<DriveFileRevision, String>;
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum OnboardingApproval {
    Original {
        #[serde(flatten)]
        approval: OriginalRegistrationApproval,
    },
    DeviceArtifact {
        #[serde(flatten)]
        approval: DeviceArtifactBindingApproval,
    },
}

pub trait OnboardingApprovalStore: Send + Sync {
    fn load(&self, drive_file_id: &str) -> Result<Option<OnboardingApproval>, String>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionedCheckpoint {
    pub value: DriveGatewayCheckpoint,
    pub version: Option<String>,
}

pub trait CheckpointStore: Send + Sync {
    fn load(&self) -> Result<VersionedCheckpoint, String>;
    fn compare_and_swap(
        &self,
        expected_version: Option<&str>,
        value: &DriveGatewayCheckpoint,
    ) -> Result<VersionedCheckpoint, String>;
}

pub trait EvidenceStore: Send + Sync {
    fn put_immutable(
        &self,
        path: &str,
        bytes: &[u8],
        metadata: &BTreeMap<String, String>,
    ) -> Result<StoredObject, String>;
    fn read_generation(&self, path: &str, generation: u64) -> Result<Option<StoredObject>, String>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrokerDisposition {
    Accepted,
    Rejected { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrokerResult {
    pub disposition: BrokerDisposition,
    pub output: Option<BrokerDriveOutput>,
}

pub trait BrokerPort: Send + Sync {
    fn register_original(
        &self,
        registration: &PreparedOriginalRegistration,
        evidence_generation: u64,
    ) -> Result<(), String>;
    fn frontier(&self, document_id: &str) -> Result<RevisionPair, String>;
    fn process(&self, pending: &PendingDriveInput) -> Result<BrokerResult, String>;
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RunReport {
    pub initialized_page_token: bool,
    pub pages_processed: usize,
    pub changes_seen: usize,
    pub inputs_applied: usize,
    pub inputs_rejected: usize,
    pub outputs_delivered: usize,
    pub duplicates: usize,
    pub ignored: usize,
    pub unbound: Vec<String>,
    pub dry_run_actions: Vec<String>,
}

pub struct GatewayJob {
    config: DriveGatewayConfig,
    drive: Arc<dyn DriveApi>,
    checkpoints: Arc<dyn CheckpointStore>,
    evidence: Arc<dyn EvidenceStore>,
    broker: Arc<dyn BrokerPort>,
    approvals: Arc<dyn OnboardingApprovalStore>,
}

impl GatewayJob {
    pub fn new(
        config: DriveGatewayConfig,
        drive: Arc<dyn DriveApi>,
        checkpoints: Arc<dyn CheckpointStore>,
        evidence: Arc<dyn EvidenceStore>,
        broker: Arc<dyn BrokerPort>,
        approvals: Arc<dyn OnboardingApprovalStore>,
    ) -> Result<Self, String> {
        config.validate()?;
        Ok(Self {
            config,
            drive,
            checkpoints,
            evidence,
            broker,
            approvals,
        })
    }

    pub fn run_once(&self, mode: RunMode) -> Result<RunReport, String> {
        let mut report = RunReport::default();
        let mut stored = self.checkpoints.load()?;
        stored.value.validate()?;

        if mode == RunMode::Apply {
            stored = self.recover_pending_inputs(stored, &mut report)?;
            stored = self.recover_pending_outputs(stored, &mut report)?;
        } else {
            for pending in stored.value.pending_drive_inputs.values() {
                report
                    .dry_run_actions
                    .push(format!("resume pending input {}", pending.drive_event_id));
            }
            for pending in stored.value.pending_drive_outputs.values() {
                report
                    .dry_run_actions
                    .push(format!("resume pending output {}", pending.delivery_id));
            }
        }

        let (changes, successor, cursor_description) = if let Some(page_token) =
            stored.value.next_page_token.clone()
        {
            let page = self.drive.list_changes(&page_token)?;
            let successor = page.durable_successor()?.to_owned();
            report.pages_processed = 1;
            (page.changes, successor, format!("page token {page_token}"))
        } else {
            // Capture the change-feed cursor before enumerating the current folders. Any
            // revision that races with this bootstrap is therefore present either in the
            // initial snapshot or in the later change feed (and may safely appear in both).
            let start_token = self.drive.start_page_token()?;
            if start_token.trim().is_empty() {
                return Err("Drive returned an empty start page token".to_owned());
            }
            let files = self.drive.list_initial_files(&[
                self.config.boox_folder_id.clone(),
                self.config.supernote_folder_id.clone(),
            ])?;
            report.initialized_page_token = true;
            report.pages_processed = 1;
            if mode == RunMode::DryRun {
                report.dry_run_actions.push(format!(
                        "inspect {} existing device-folder files before initializing Drive change cursor",
                        files.len()
                    ));
            }
            (
                files.into_iter().map(|file| DriveChange { file }).collect(),
                start_token,
                "initial folder snapshot".to_owned(),
            )
        };

        let mut simulated_frontiers = BTreeMap::new();
        for change in self.order_changes(changes, &stored.value)? {
            report.changes_seen += 1;
            stored =
                self.process_change(stored, change, mode, &mut simulated_frontiers, &mut report)?;
        }

        if mode == RunMode::DryRun {
            report.dry_run_actions.push(format!(
                "retain {cursor_description}; dry-run never checkpoints"
            ));
        } else {
            commit_page_token(&mut stored.value, successor);
            let _ = self.persist(stored)?;
        }
        Ok(report)
    }

    fn order_changes(
        &self,
        changes: Vec<DriveChange>,
        checkpoint: &DriveGatewayCheckpoint,
    ) -> Result<Vec<DriveChange>, String> {
        let mut approved_originals = Vec::new();
        let mut remaining = Vec::new();
        for change in changes {
            let is_unbound = checkpoint.binding_for_file(&change.file.file_id).is_none();
            let is_approved_original = is_unbound
                && matches!(
                    self.approvals.load(&change.file.file_id)?,
                    Some(OnboardingApproval::Original { .. })
                );
            if is_approved_original {
                approved_originals.push(change);
            } else {
                remaining.push(change);
            }
        }
        approved_originals.extend(remaining);
        Ok(approved_originals)
    }

    fn process_change(
        &self,
        mut stored: VersionedCheckpoint,
        change: DriveChange,
        mode: RunMode,
        simulated_frontiers: &mut BTreeMap<String, RevisionPair>,
        report: &mut RunReport,
    ) -> Result<VersionedCheckpoint, String> {
        if stored
            .value
            .binding_for_file(&change.file.file_id)
            .is_none()
        {
            match prepare_drive_input(
                &self.config,
                &stored.value,
                &change,
                &[],
                CanonicalFrontier {
                    revisions: RevisionPair::default(),
                },
            )? {
                DriveInputDecision::Ignore { .. } => {
                    report.ignored += 1;
                    return Ok(stored);
                }
                DriveInputDecision::Unbound { .. } => {
                    let bytes = self.download_verified(&change.file)?;
                    let Some(approval) = self.approvals.load(&change.file.file_id)? else {
                        return self.block_unapproved(stored, &change.file.file_id, mode, report);
                    };
                    match approval {
                        OnboardingApproval::Original { approval } => {
                            match prepare_original_registration(
                                &self.config,
                                &stored.value,
                                &change,
                                &bytes,
                                &approval,
                            )? {
                                RegistrationDecision::Register(registration) => {
                                    if mode == RunMode::DryRun {
                                        commit_original_registration(
                                            &mut stored.value,
                                            &registration,
                                        )?;
                                        simulated_frontiers.insert(
                                            registration.document_id.clone(),
                                            RevisionPair::default(),
                                        );
                                        report.dry_run_actions.push(format!(
                                            "register approved original {} as {}",
                                            registration.drive_file_id, registration.document_id
                                        ));
                                    } else {
                                        let object = self.evidence.put_immutable(
                                            &registration.gcs_object_path,
                                            &bytes,
                                            &registration.metadata,
                                        )?;
                                        if object.generation == 0 {
                                            return Err(
                                                "original registration evidence has generation zero"
                                                .to_owned(),
                                            );
                                        }
                                        self.broker
                                            .register_original(&registration, object.generation)?;
                                        commit_original_registration(
                                            &mut stored.value,
                                            &registration,
                                        )?;
                                        stored = self.persist(stored)?;
                                    }
                                    return Ok(stored);
                                }
                                RegistrationDecision::Duplicate { .. } => {
                                    report.duplicates += 1;
                                    return Ok(stored);
                                }
                                RegistrationDecision::Ignore { reason } => {
                                    return self.block_invalid_approval(
                                        stored,
                                        &change.file.file_id,
                                        &reason,
                                        mode,
                                        report,
                                    );
                                }
                            }
                        }
                        OnboardingApproval::DeviceArtifact { approval } => {
                            let frontier =
                                self.frontier(&approval.document_id, simulated_frontiers)?;
                            match prepare_device_artifact_binding(
                                &self.config,
                                &stored.value,
                                &change,
                                &bytes,
                                &approval,
                                CanonicalFrontier {
                                    revisions: frontier,
                                },
                            )? {
                                DeviceArtifactBindingDecision::Bind(binding) => {
                                    commit_device_artifact_binding(&mut stored.value, &binding)?;
                                    if mode == RunMode::DryRun {
                                        report.dry_run_actions.push(format!(
                                            "bind approved device artifact {} to {}",
                                            binding.drive_file_id, binding.document_id
                                        ));
                                    } else {
                                        stored = self.persist(stored)?;
                                    }
                                }
                                DeviceArtifactBindingDecision::AlreadyBound { .. } => {}
                                DeviceArtifactBindingDecision::Ignore { reason } => {
                                    return self.block_invalid_approval(
                                        stored,
                                        &change.file.file_id,
                                        &reason,
                                        mode,
                                        report,
                                    );
                                }
                            }
                        }
                    }
                    return self.process_bound_change(
                        stored,
                        change,
                        bytes,
                        mode,
                        simulated_frontiers,
                        report,
                    );
                }
                other => {
                    return Err(format!(
                        "unbound Drive file produced an invalid decision: {other:?}"
                    ));
                }
            }
        }

        let bytes = if change.file.trashed {
            Vec::new()
        } else {
            self.download_verified(&change.file)?
        };
        self.process_bound_change(stored, change, bytes, mode, simulated_frontiers, report)
    }

    fn frontier(
        &self,
        document_id: &str,
        simulated_frontiers: &BTreeMap<String, RevisionPair>,
    ) -> Result<RevisionPair, String> {
        simulated_frontiers
            .get(document_id)
            .copied()
            .map(Ok)
            .unwrap_or_else(|| self.broker.frontier(document_id))
    }

    fn process_bound_change(
        &self,
        mut stored: VersionedCheckpoint,
        change: DriveChange,
        bytes: Vec<u8>,
        mode: RunMode,
        simulated_frontiers: &BTreeMap<String, RevisionPair>,
        report: &mut RunReport,
    ) -> Result<VersionedCheckpoint, String> {
        let binding = stored
            .value
            .binding_for_file(&change.file.file_id)
            .ok_or_else(|| "Drive binding disappeared".to_owned())?;
        let frontier = self.frontier(&binding.document_id, simulated_frontiers)?;
        match prepare_drive_input(
            &self.config,
            &stored.value,
            &change,
            &bytes,
            CanonicalFrontier {
                revisions: frontier,
            },
        )? {
            DriveInputDecision::Ignore { .. } => report.ignored += 1,
            DriveInputDecision::Duplicate { .. } => report.duplicates += 1,
            DriveInputDecision::Unbound { file_id } => {
                return Err(format!("bound Drive file became unbound: {file_id}"));
            }
            DriveInputDecision::Deferred {
                pending_drive_event_id,
                ..
            } => {
                return Err(format!(
                    "Drive input is still blocked by pending event {pending_drive_event_id}"
                ));
            }
            DriveInputDecision::Upload(input) => {
                if mode == RunMode::DryRun {
                    report.dry_run_actions.push(format!(
                        "upload {} as immutable evidence {}",
                        input.drive_event_id, input.gcs_object_path
                    ));
                    return Ok(stored);
                }
                let object =
                    self.evidence
                        .put_immutable(&input.gcs_object_path, &bytes, &input.metadata)?;
                commit_drive_input(&mut stored.value, &input, object.generation)?;
                stored = self.persist(stored)?;
                let pending = stored
                    .value
                    .pending_drive_inputs
                    .get(&input.drive_event_id)
                    .cloned()
                    .ok_or_else(|| "committed Drive input disappeared".to_owned())?;
                stored = self.finish_pending_input(stored, &pending, report)?;
            }
        }
        Ok(stored)
    }

    fn download_verified(&self, listed: &DriveFileRevision) -> Result<Vec<u8>, String> {
        let bytes = self.drive.download(&listed.file_id)?;
        let current = self.drive.file_revision(&listed.file_id)?;
        if &current != listed {
            return Err(format!(
                "Drive file {} changed while it was being downloaded; retry the job",
                listed.file_id
            ));
        }
        if bytes.len() as u64 != listed.size {
            return Err(format!(
                "Drive download length {} does not match declared size {}",
                bytes.len(),
                listed.size
            ));
        }
        Ok(bytes)
    }

    fn block_unapproved(
        &self,
        stored: VersionedCheckpoint,
        file_id: &str,
        mode: RunMode,
        report: &mut RunReport,
    ) -> Result<VersionedCheckpoint, String> {
        report.unbound.push(file_id.to_owned());
        if mode == RunMode::DryRun {
            report.dry_run_actions.push(format!(
                "require explicit onboarding approval for {file_id}"
            ));
            Ok(stored)
        } else {
            Err(format!(
                "Drive file {file_id} is unbound and has no explicit onboarding approval; page token retained"
            ))
        }
    }

    fn block_invalid_approval(
        &self,
        stored: VersionedCheckpoint,
        file_id: &str,
        reason: &str,
        mode: RunMode,
        report: &mut RunReport,
    ) -> Result<VersionedCheckpoint, String> {
        report.unbound.push(file_id.to_owned());
        if mode == RunMode::DryRun {
            report.dry_run_actions.push(format!(
                "reject onboarding approval for {file_id}: {reason}"
            ));
            Ok(stored)
        } else {
            Err(format!(
                "Drive onboarding approval for {file_id} is invalid: {reason}; page token retained"
            ))
        }
    }

    fn recover_pending_inputs(
        &self,
        mut stored: VersionedCheckpoint,
        report: &mut RunReport,
    ) -> Result<VersionedCheckpoint, String> {
        let pending = stored
            .value
            .pending_drive_inputs
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for input in pending {
            stored = self.finish_pending_input(stored, &input, report)?;
        }
        Ok(stored)
    }

    fn finish_pending_input(
        &self,
        mut stored: VersionedCheckpoint,
        input: &PendingDriveInput,
        report: &mut RunReport,
    ) -> Result<VersionedCheckpoint, String> {
        let result = self.broker.process(input)?;
        match result.disposition {
            BrokerDisposition::Accepted => {
                accept_drive_input(&mut stored.value, &input.drive_event_id)?;
                report.inputs_applied += 1;
                if let Some(output) = result.output {
                    stored = self.reserve_output(stored, &output)?;
                }
            }
            BrokerDisposition::Rejected { .. } => {
                reject_drive_input(&mut stored.value, &input.drive_event_id)?;
                report.inputs_rejected += 1;
            }
        }
        stored = self.persist(stored)?;
        self.recover_pending_outputs(stored, report)
    }

    fn reserve_output(
        &self,
        mut stored: VersionedCheckpoint,
        output: &BrokerDriveOutput,
    ) -> Result<VersionedCheckpoint, String> {
        match prepare_drive_output(&self.config, &stored.value, output)? {
            DriveOutputDecision::Duplicate { .. } => Ok(stored),
            DriveOutputDecision::Reserve(prepared) => {
                reserve_drive_output(&mut stored.value, &prepared)?;
                Ok(stored)
            }
            DriveOutputDecision::Reconcile(_) => Ok(stored),
            other => Err(format!(
                "unexpected output decision before reservation: {other:?}"
            )),
        }
    }

    fn recover_pending_outputs(
        &self,
        mut stored: VersionedCheckpoint,
        report: &mut RunReport,
    ) -> Result<VersionedCheckpoint, String> {
        let outputs = stored
            .value
            .pending_drive_outputs
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for output in outputs {
            let matches = self.drive.find_delivery(&output.delivery_id)?;
            let (file_id, version) =
                match reconcile_drive_output(&self.config, &stored.value, &output, &matches)? {
                    DriveOutputDecision::Create(create) => {
                        let object = self
                            .evidence
                            .read_generation(&create.gcs_object_path, create.gcs_generation)?
                            .ok_or_else(|| {
                                format!(
                                    "broker output {} generation {} is missing",
                                    create.gcs_object_path, create.gcs_generation
                                )
                            })?;
                        if sha256_hex(&object.bytes) != create.content_sha256 {
                            return Err(format!(
                                "broker output {} hash does not match its reservation",
                                create.delivery_id
                            ));
                        }
                        let created = self.drive.create_delivery(&create, &object.bytes)?;
                        (created.file_id, created.version)
                    }
                    DriveOutputDecision::Existing {
                        drive_file_id,
                        drive_file_version,
                        ..
                    } => (drive_file_id, drive_file_version),
                    other => {
                        return Err(format!(
                            "unexpected output reconciliation decision: {other:?}"
                        ));
                    }
                };
            commit_drive_output(&mut stored.value, &output, file_id, version)?;
            stored = self.persist(stored)?;
            report.outputs_delivered += 1;
        }
        Ok(stored)
    }

    fn persist(&self, stored: VersionedCheckpoint) -> Result<VersionedCheckpoint, String> {
        stored.value.validate()?;
        self.checkpoints
            .compare_and_swap(stored.version.as_deref(), &stored.value)
    }
}

#[derive(Clone)]
pub struct CloudEvidenceStore {
    objects: Arc<dyn ObjectStore>,
}

impl CloudEvidenceStore {
    pub fn new(objects: Arc<dyn ObjectStore>) -> Self {
        Self { objects }
    }
}

impl EvidenceStore for CloudEvidenceStore {
    fn put_immutable(
        &self,
        path: &str,
        bytes: &[u8],
        metadata: &BTreeMap<String, String>,
    ) -> Result<StoredObject, String> {
        let write = ConditionalWrite {
            path: path.to_owned(),
            bytes: inkbridge_broker::blob(bytes.to_vec()),
            metadata: metadata.clone(),
            precondition: GenerationPrecondition::DoesNotExist,
        };
        match self.objects.conditional_write(&write) {
            Ok(object) => Ok(object),
            Err(CommitError::PreconditionFailed { actual, .. }) => {
                let generation = actual.ok_or_else(|| {
                    format!("immutable evidence {path} already exists at an unknown generation")
                })?;
                let existing = self
                    .objects
                    .read_generation(path, generation)?
                    .ok_or_else(|| format!("immutable evidence {path}@{generation} disappeared"))?;
                if existing.bytes.as_ref() != bytes || existing.metadata != *metadata {
                    return Err(format!(
                        "immutable evidence {path}@{generation} differs from the retried upload"
                    ));
                }
                Ok(existing)
            }
            Err(CommitError::Other(reason)) => Err(reason),
        }
    }

    fn read_generation(&self, path: &str, generation: u64) -> Result<Option<StoredObject>, String> {
        self.objects.read_generation(path, generation)
    }
}

#[derive(Clone)]
pub struct CloudBrokerPort {
    bucket: String,
    service: RuntimeService,
    objects: Arc<dyn ObjectStore>,
    states: Arc<dyn CanonicalStateStore>,
}

impl CloudBrokerPort {
    pub fn new(
        bucket: impl Into<String>,
        objects: Arc<dyn ObjectStore>,
        states: Arc<dyn CanonicalStateStore>,
    ) -> Self {
        let bucket = bucket.into();
        Self {
            service: RuntimeService::new(bucket.clone(), objects.clone(), states.clone()),
            bucket,
            objects,
            states,
        }
    }

    fn state(&self, document_id: &str) -> Result<CanonicalDocumentState, String> {
        let record = self.states.load(document_id)?;
        let active = record
            .active
            .ok_or_else(|| format!("canonical state does not exist for {document_id}"))?;
        let object = self
            .objects
            .read_generation(&active.payload.path, active.payload.generation)?
            .ok_or_else(|| {
                format!(
                    "canonical state payload {}@{} is missing",
                    active.payload.path, active.payload.generation
                )
            })?;
        serde_json::from_slice(&object.bytes).map_err(|error| error.to_string())
    }

    fn output_for_event(
        &self,
        state: &CanonicalDocumentState,
        event_id: &str,
    ) -> Result<Option<BrokerDriveOutput>, String> {
        let Some(view) = state
            .generated_views
            .values()
            .find(|view| view.event_id == event_id)
        else {
            return Ok(None);
        };
        let object = self
            .objects
            .read(&view.object_path)?
            .ok_or_else(|| format!("generated broker view {} is missing", view.object_path))?;
        if sha256_hex(&object.bytes) != view.content_sha256 {
            return Err(format!(
                "generated broker view {} no longer matches canonical state",
                view.object_path
            ));
        }
        let target = if view.object_path.starts_with("BOOX_Folder/") {
            DeviceSide::Boox
        } else if view.object_path.starts_with("Supernote_Folder/") {
            DeviceSide::Supernote
        } else {
            return Err(format!(
                "generated broker view {} is outside device folders",
                view.object_path
            ));
        };
        let extension = Path::new(&view.object_path)
            .extension()
            .and_then(|value| value.to_str())
            .ok_or_else(|| "generated broker view has no extension".to_owned())?
            .to_owned();
        Ok(Some(BrokerDriveOutput {
            gcs_object_path: view.object_path.clone(),
            gcs_generation: object.generation,
            document_id: state.document_id.clone(),
            target,
            event_id: event_id.to_owned(),
            source_revisions: view.source_revisions,
            content_sha256: view.content_sha256.clone(),
            file_extension: extension,
        }))
    }
}

impl BrokerPort for CloudBrokerPort {
    fn register_original(
        &self,
        registration: &PreparedOriginalRegistration,
        evidence_generation: u64,
    ) -> Result<(), String> {
        let original_file_name = registration
            .metadata
            .get("inkbridge-original-file-name")
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "original registration has no source file name".to_owned())?;
        let state = self.service.register_document(&RegisterDocumentRequest {
            original_object_path: registration.gcs_object_path.clone(),
            original_file_name: original_file_name.clone(),
            source_generation: Some(evidence_generation),
        })?;
        if state.document_id != registration.document_id
            || state.original_pdf_sha256 != registration.original_pdf_sha256
        {
            return Err("broker registered a different original document identity".to_owned());
        }
        Ok(())
    }

    fn frontier(&self, document_id: &str) -> Result<RevisionPair, String> {
        self.state(document_id).map(|state| state.revisions())
    }

    fn process(&self, pending: &PendingDriveInput) -> Result<BrokerResult, String> {
        let object = self
            .objects
            .read_generation(&pending.gcs_object_path, pending.gcs_generation)?
            .ok_or_else(|| {
                format!(
                    "pending input {}@{} is missing",
                    pending.gcs_object_path, pending.gcs_generation
                )
            })?;
        let body = serde_json::to_vec(&json!({
            "bucket": self.bucket,
            "name": pending.gcs_object_path,
            "generation": pending.gcs_generation.to_string(),
            "size": object.bytes.len().to_string(),
            "metadata": object.metadata,
        }))
        .map_err(|error| error.to_string())?;
        let headers = BTreeMap::from([
            (
                "ce-type".to_owned(),
                "google.cloud.storage.object.v1.finalized".to_owned(),
            ),
            ("ce-id".to_owned(), pending.drive_event_id.clone()),
        ]);
        let outcome = self.service.handle_storage_event(&headers, &body)?;
        match outcome {
            RuntimeOutcome::Rejected { reason } => Ok(BrokerResult {
                disposition: BrokerDisposition::Rejected { reason },
                output: None,
            }),
            RuntimeOutcome::Ignored { reason } => Err(format!(
                "broker unexpectedly ignored pending Drive input: {reason}"
            )),
            RuntimeOutcome::Registered { .. } => {
                Err("broker unexpectedly registered a pending Drive input".to_owned())
            }
            RuntimeOutcome::Processed { outcome } => {
                let event_id = match &outcome {
                    ProcessOutcome::Applied { event_id, .. }
                    | ProcessOutcome::Duplicate { event_id, .. }
                    | ProcessOutcome::IgnoredBrokerOutput { event_id, .. }
                    | ProcessOutcome::IgnoredStaleSource { event_id, .. }
                    | ProcessOutcome::Conflict { event_id, .. } => event_id,
                };
                if event_id != &pending.drive_event_id {
                    return Err("broker outcome event identity changed".to_owned());
                }
                if matches!(&outcome, ProcessOutcome::IgnoredBrokerOutput { .. }) {
                    return Err(
                        "broker treated a device-originated pending input as its own output"
                            .to_owned(),
                    );
                }
                let requires_output = matches!(
                    &outcome,
                    ProcessOutcome::Applied { .. } | ProcessOutcome::Duplicate { .. }
                );
                let state = self.state(&pending.document_id)?;
                let output = self.output_for_event(&state, event_id)?;
                if requires_output && output.is_none() {
                    return Err(format!(
                        "broker accepted {} but its exact generated delivery is unavailable",
                        pending.drive_event_id
                    ));
                }
                Ok(BrokerResult {
                    disposition: BrokerDisposition::Accepted,
                    output,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use inkbridge_drive_gateway::{
        DocumentBinding, DRIVE_GATEWAY_PRODUCER, DRIVE_GATEWAY_SCHEMA_VERSION,
    };
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Mutex;

    const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn document_id() -> String {
        format!("inkbridge-doc-v1-{HASH}")
    }

    fn config() -> DriveGatewayConfig {
        DriveGatewayConfig {
            schema_version: DRIVE_GATEWAY_SCHEMA_VERSION,
            boox_folder_id: "boox-folder-123".to_owned(),
            supernote_folder_id: "supernote-folder-123".to_owned(),
        }
    }

    fn checkpoint() -> DriveGatewayCheckpoint {
        let document_id = document_id();
        DriveGatewayCheckpoint {
            schema_version: DRIVE_GATEWAY_SCHEMA_VERSION,
            next_page_token: Some("page-1".to_owned()),
            documents: BTreeMap::from([(
                document_id.clone(),
                DocumentBinding {
                    document_id,
                    original_pdf_sha256: HASH.to_owned(),
                    boox_file_ids: BTreeSet::from(["boox-file".to_owned()]),
                    supernote_file_ids: BTreeSet::from(["supernote-file".to_owned()]),
                },
            )]),
            file_observed_frontiers: BTreeMap::from([
                ("boox-file".to_owned(), RevisionPair::default()),
                ("supernote-file".to_owned(), RevisionPair::default()),
            ]),
            ..DriveGatewayCheckpoint::empty()
        }
    }

    fn change(bytes: &[u8]) -> DriveChange {
        DriveChange {
            file: DriveFileRevision {
                file_id: "boox-file".to_owned(),
                name: "book.json".to_owned(),
                version: 2,
                mime_type: "application/json".to_owned(),
                parents: vec!["boox-folder-123".to_owned()],
                size: bytes.len() as u64,
                trashed: false,
                app_properties: BTreeMap::new(),
            },
        }
    }

    #[derive(Default)]
    struct FakeDrive {
        page: Mutex<Option<DriveChangePage>>,
        initial_files: Mutex<Vec<DriveFileRevision>>,
        current_revisions: Mutex<BTreeMap<String, DriveFileRevision>>,
        downloads: Mutex<BTreeMap<String, Vec<u8>>>,
        deliveries: Mutex<Vec<DriveFileRevision>>,
        creates: Mutex<usize>,
    }

    impl FakeDrive {
        fn with_page(bytes: &[u8]) -> Self {
            Self {
                page: Mutex::new(Some(DriveChangePage {
                    changes: vec![change(bytes)],
                    next_page_token: None,
                    new_start_page_token: Some("page-2".to_owned()),
                })),
                downloads: Mutex::new(BTreeMap::from([("boox-file".to_owned(), bytes.to_vec())])),
                ..Self::default()
            }
        }
    }

    impl DriveApi for FakeDrive {
        fn start_page_token(&self) -> Result<String, String> {
            Ok("initial".to_owned())
        }

        fn list_initial_files(
            &self,
            _folder_ids: &[String],
        ) -> Result<Vec<DriveFileRevision>, String> {
            Ok(self.initial_files.lock().unwrap().clone())
        }

        fn list_changes(&self, _page_token: &str) -> Result<DriveChangePage, String> {
            self.page
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| "no page".to_owned())
        }

        fn download(&self, file_id: &str) -> Result<Vec<u8>, String> {
            self.downloads
                .lock()
                .unwrap()
                .get(file_id)
                .cloned()
                .ok_or_else(|| "missing download".to_owned())
        }

        fn file_revision(&self, file_id: &str) -> Result<DriveFileRevision, String> {
            if let Some(revision) = self.current_revisions.lock().unwrap().get(file_id).cloned() {
                return Ok(revision);
            }
            self.page
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|page| {
                    page.changes
                        .iter()
                        .find(|change| change.file.file_id == file_id)
                })
                .map(|change| change.file.clone())
                .or_else(|| {
                    self.initial_files
                        .lock()
                        .unwrap()
                        .iter()
                        .find(|file| file.file_id == file_id)
                        .cloned()
                })
                .ok_or_else(|| "missing Drive file revision".to_owned())
        }

        fn find_delivery(&self, delivery_id: &str) -> Result<Vec<DriveFileRevision>, String> {
            Ok(self
                .deliveries
                .lock()
                .unwrap()
                .iter()
                .filter(|file| {
                    file.app_properties
                        .get("inkbridgeDeliveryId")
                        .map(String::as_str)
                        == Some(delivery_id)
                })
                .cloned()
                .collect())
        }

        fn create_delivery(
            &self,
            output: &PreparedDriveOutput,
            bytes: &[u8],
        ) -> Result<DriveFileRevision, String> {
            *self.creates.lock().unwrap() += 1;
            let file = DriveFileRevision {
                file_id: format!("created-{}", output.delivery_id),
                name: output.file_name.clone(),
                version: 9,
                mime_type: if output.file_name.ends_with(".pdf") {
                    "application/pdf".to_owned()
                } else {
                    "application/json".to_owned()
                },
                parents: vec![output.parent_folder_id.clone()],
                size: bytes.len() as u64,
                trashed: false,
                app_properties: output.app_properties.clone(),
            };
            self.deliveries.lock().unwrap().push(file.clone());
            Ok(file)
        }
    }

    struct FakeCheckpoints {
        inner: Mutex<FakeCheckpointState>,
    }

    struct FakeCheckpointState {
        value: DriveGatewayCheckpoint,
        version: u64,
        cas_calls: usize,
        fail_on_call: Option<usize>,
    }

    impl FakeCheckpoints {
        fn new(value: DriveGatewayCheckpoint) -> Self {
            Self {
                inner: Mutex::new(FakeCheckpointState {
                    value,
                    version: 1,
                    cas_calls: 0,
                    fail_on_call: None,
                }),
            }
        }

        fn fail_on(&self, call: usize) {
            self.inner.lock().unwrap().fail_on_call = Some(call);
        }

        fn value(&self) -> DriveGatewayCheckpoint {
            self.inner.lock().unwrap().value.clone()
        }

        fn clear_failure(&self) {
            self.inner.lock().unwrap().fail_on_call = None;
        }
    }

    impl CheckpointStore for FakeCheckpoints {
        fn load(&self) -> Result<VersionedCheckpoint, String> {
            let inner = self.inner.lock().unwrap();
            Ok(VersionedCheckpoint {
                value: inner.value.clone(),
                version: Some(inner.version.to_string()),
            })
        }

        fn compare_and_swap(
            &self,
            expected_version: Option<&str>,
            value: &DriveGatewayCheckpoint,
        ) -> Result<VersionedCheckpoint, String> {
            let mut inner = self.inner.lock().unwrap();
            inner.cas_calls += 1;
            if inner.fail_on_call == Some(inner.cas_calls) {
                return Err("injected stale checkpoint generation".to_owned());
            }
            if expected_version != Some(inner.version.to_string().as_str()) {
                return Err("stale checkpoint generation".to_owned());
            }
            inner.version += 1;
            inner.value = value.clone();
            Ok(VersionedCheckpoint {
                value: value.clone(),
                version: Some(inner.version.to_string()),
            })
        }
    }

    #[derive(Default)]
    struct FakeEvidence {
        objects: Mutex<BTreeMap<String, StoredObject>>,
        next_generation: Mutex<u64>,
    }

    impl FakeEvidence {
        fn insert(&self, path: &str, bytes: &[u8], generation: u64) {
            self.objects.lock().unwrap().insert(
                path.to_owned(),
                StoredObject {
                    bytes: inkbridge_broker::blob(bytes.to_vec()),
                    generation,
                    metadata: BTreeMap::new(),
                },
            );
        }
    }

    impl EvidenceStore for FakeEvidence {
        fn put_immutable(
            &self,
            path: &str,
            bytes: &[u8],
            metadata: &BTreeMap<String, String>,
        ) -> Result<StoredObject, String> {
            if let Some(existing) = self.objects.lock().unwrap().get(path).cloned() {
                if existing.bytes.as_ref() == bytes && existing.metadata == *metadata {
                    return Ok(existing);
                }
                return Err("immutable evidence collision".to_owned());
            }
            let mut next = self.next_generation.lock().unwrap();
            *next += 1;
            let object = StoredObject {
                bytes: inkbridge_broker::blob(bytes.to_vec()),
                generation: *next,
                metadata: metadata.clone(),
            };
            self.objects
                .lock()
                .unwrap()
                .insert(path.to_owned(), object.clone());
            Ok(object)
        }

        fn read_generation(
            &self,
            path: &str,
            generation: u64,
        ) -> Result<Option<StoredObject>, String> {
            Ok(self
                .objects
                .lock()
                .unwrap()
                .get(path)
                .filter(|object| object.generation == generation)
                .cloned())
        }
    }

    struct FakeBroker {
        fail_once: Mutex<bool>,
        output: Option<BrokerDriveOutput>,
    }

    #[derive(Default)]
    struct FakeApprovals {
        values: Mutex<BTreeMap<String, OnboardingApproval>>,
    }

    impl OnboardingApprovalStore for FakeApprovals {
        fn load(&self, drive_file_id: &str) -> Result<Option<OnboardingApproval>, String> {
            Ok(self.values.lock().unwrap().get(drive_file_id).cloned())
        }
    }

    impl BrokerPort for FakeBroker {
        fn register_original(
            &self,
            _registration: &PreparedOriginalRegistration,
            _evidence_generation: u64,
        ) -> Result<(), String> {
            Ok(())
        }

        fn frontier(&self, _document_id: &str) -> Result<RevisionPair, String> {
            Ok(RevisionPair::default())
        }

        fn process(&self, _pending: &PendingDriveInput) -> Result<BrokerResult, String> {
            let mut fail = self.fail_once.lock().unwrap();
            if *fail {
                *fail = false;
                return Err("injected broker interruption".to_owned());
            }
            Ok(BrokerResult {
                disposition: BrokerDisposition::Accepted,
                output: self.output.clone(),
            })
        }
    }

    struct OrderingBroker {
        registered: Mutex<BTreeSet<String>>,
    }

    impl BrokerPort for OrderingBroker {
        fn register_original(
            &self,
            registration: &PreparedOriginalRegistration,
            _evidence_generation: u64,
        ) -> Result<(), String> {
            self.registered
                .lock()
                .unwrap()
                .insert(registration.document_id.clone());
            Ok(())
        }

        fn frontier(&self, document_id: &str) -> Result<RevisionPair, String> {
            if !self.registered.lock().unwrap().contains(document_id) {
                return Err("canonical document is not registered".to_owned());
            }
            Ok(RevisionPair::default())
        }

        fn process(&self, _pending: &PendingDriveInput) -> Result<BrokerResult, String> {
            Ok(BrokerResult {
                disposition: BrokerDisposition::Accepted,
                output: None,
            })
        }
    }

    struct RejectingRegistrationBroker;

    impl BrokerPort for RejectingRegistrationBroker {
        fn register_original(
            &self,
            _registration: &PreparedOriginalRegistration,
            _evidence_generation: u64,
        ) -> Result<(), String> {
            Err("broker rejected malformed original PDF".to_owned())
        }

        fn frontier(&self, _document_id: &str) -> Result<RevisionPair, String> {
            Err("unregistered document has no frontier".to_owned())
        }

        fn process(&self, _pending: &PendingDriveInput) -> Result<BrokerResult, String> {
            Err("unregistered document cannot process annotations".to_owned())
        }
    }

    fn job(
        drive: Arc<FakeDrive>,
        checkpoints: Arc<FakeCheckpoints>,
        evidence: Arc<FakeEvidence>,
        broker: Arc<FakeBroker>,
    ) -> GatewayJob {
        job_with_approvals(
            drive,
            checkpoints,
            evidence,
            broker,
            Arc::new(FakeApprovals::default()),
        )
    }

    fn job_with_approvals(
        drive: Arc<FakeDrive>,
        checkpoints: Arc<FakeCheckpoints>,
        evidence: Arc<FakeEvidence>,
        broker: Arc<FakeBroker>,
        approvals: Arc<FakeApprovals>,
    ) -> GatewayJob {
        GatewayJob::new(config(), drive, checkpoints, evidence, broker, approvals).unwrap()
    }

    #[test]
    fn broker_failure_keeps_page_token_and_pending_evidence_for_retry() {
        let bytes = br#"{"schemaVersion":1}"#;
        let drive = Arc::new(FakeDrive::with_page(bytes));
        let checkpoints = Arc::new(FakeCheckpoints::new(checkpoint()));
        let evidence = Arc::new(FakeEvidence::default());
        let broker = Arc::new(FakeBroker {
            fail_once: Mutex::new(true),
            output: None,
        });
        let job = job(drive, checkpoints.clone(), evidence, broker);

        assert!(job.run_once(RunMode::Apply).is_err());
        let interrupted = checkpoints.value();
        assert_eq!(interrupted.next_page_token.as_deref(), Some("page-1"));
        assert_eq!(interrupted.pending_drive_inputs.len(), 1);

        let report = job.run_once(RunMode::Apply).unwrap();
        assert_eq!(report.inputs_applied, 1);
        assert_eq!(
            checkpoints.value().next_page_token.as_deref(),
            Some("page-2")
        );
        assert!(checkpoints.value().pending_drive_inputs.is_empty());
    }

    #[test]
    fn crash_after_drive_create_reconciles_without_duplicate_file() {
        let bytes = br#"{"schemaVersion":1}"#;
        let drive = Arc::new(FakeDrive::with_page(bytes));
        let checkpoints = Arc::new(FakeCheckpoints::new(checkpoint()));
        // CAS 1 commits pending input; CAS 2 accepts it and reserves output;
        // CAS 3 is after Drive files.create and is interrupted.
        checkpoints.fail_on(3);
        let evidence = Arc::new(FakeEvidence::default());
        let output_bytes = b"supernote manifest";
        let output_path = format!("Supernote_Folder/{}/incoming.json", document_id());
        evidence.insert(&output_path, output_bytes, 44);
        let broker = Arc::new(FakeBroker {
            fail_once: Mutex::new(false),
            output: Some(BrokerDriveOutput {
                gcs_object_path: output_path,
                gcs_generation: 44,
                document_id: document_id(),
                target: DeviceSide::Supernote,
                event_id: "drive-event".to_owned(),
                source_revisions: RevisionPair {
                    boox: 1,
                    supernote: 0,
                },
                content_sha256: sha256_hex(output_bytes),
                file_extension: "json".to_owned(),
            }),
        });
        let job = job(drive.clone(), checkpoints.clone(), evidence, broker);

        assert!(job.run_once(RunMode::Apply).is_err());
        assert_eq!(*drive.creates.lock().unwrap(), 1);
        assert_eq!(checkpoints.value().pending_drive_outputs.len(), 1);

        checkpoints.clear_failure();
        let report = job.run_once(RunMode::Apply).unwrap();
        assert_eq!(report.outputs_delivered, 1);
        assert_eq!(*drive.creates.lock().unwrap(), 1);
        assert!(checkpoints.value().pending_drive_outputs.is_empty());
        assert_eq!(checkpoints.value().delivered_broker_outputs.len(), 1);
    }

    #[test]
    fn duplicate_content_does_not_consume_another_source_revision() {
        let bytes = br#"{"schemaVersion":1}"#;
        let drive = Arc::new(FakeDrive::with_page(bytes));
        let mut initial = checkpoint();
        initial
            .accepted_file_content_sha256
            .insert("boox-file".to_owned(), sha256_hex(bytes));
        let checkpoints = Arc::new(FakeCheckpoints::new(initial));
        let job = job(
            drive,
            checkpoints.clone(),
            Arc::new(FakeEvidence::default()),
            Arc::new(FakeBroker {
                fail_once: Mutex::new(false),
                output: None,
            }),
        );

        let report = job.run_once(RunMode::Apply).unwrap();
        assert_eq!(report.duplicates, 1);
        assert_eq!(
            checkpoints.value().file_observed_frontiers["boox-file"],
            RevisionPair::default()
        );
        assert_eq!(
            checkpoints.value().next_page_token.as_deref(),
            Some("page-2")
        );
    }

    #[test]
    fn stale_checkpoint_write_never_advances_page_token() {
        let bytes = br#"{"schemaVersion":1}"#;
        let drive = Arc::new(FakeDrive::with_page(bytes));
        let mut initial = checkpoint();
        initial
            .accepted_file_content_sha256
            .insert("boox-file".to_owned(), sha256_hex(bytes));
        let checkpoints = Arc::new(FakeCheckpoints::new(initial));
        checkpoints.fail_on(1);
        let job = job(
            drive,
            checkpoints.clone(),
            Arc::new(FakeEvidence::default()),
            Arc::new(FakeBroker {
                fail_once: Mutex::new(false),
                output: None,
            }),
        );

        assert!(job.run_once(RunMode::Apply).is_err());
        assert_eq!(
            checkpoints.value().next_page_token.as_deref(),
            Some("page-1")
        );
    }

    #[test]
    fn dry_run_never_changes_checkpoint_or_uploads_evidence() {
        let bytes = br#"{"schemaVersion":1}"#;
        let drive = Arc::new(FakeDrive::with_page(bytes));
        let checkpoints = Arc::new(FakeCheckpoints::new(checkpoint()));
        let evidence = Arc::new(FakeEvidence::default());
        let job = job(
            drive,
            checkpoints.clone(),
            evidence.clone(),
            Arc::new(FakeBroker {
                fail_once: Mutex::new(false),
                output: None,
            }),
        );

        let report = job.run_once(RunMode::DryRun).unwrap();
        assert_eq!(report.dry_run_actions.len(), 2);
        assert_eq!(
            checkpoints.value().next_page_token.as_deref(),
            Some("page-1")
        );
        assert!(evidence.objects.lock().unwrap().is_empty());
    }

    #[test]
    fn unbound_broker_generated_output_is_ignored_without_download() {
        let drive = Arc::new(FakeDrive::default());
        *drive.page.lock().unwrap() = Some(DriveChangePage {
            changes: vec![DriveChange {
                file: DriveFileRevision {
                    file_id: "unbound-generated".to_owned(),
                    name: "delivery.json".to_owned(),
                    version: 1,
                    mime_type: "application/json".to_owned(),
                    parents: vec!["supernote-folder-123".to_owned()],
                    size: 5,
                    trashed: false,
                    app_properties: BTreeMap::from([(
                        "inkbridgeGeneratedBy".to_owned(),
                        DRIVE_GATEWAY_PRODUCER.to_owned(),
                    )]),
                },
            }],
            next_page_token: None,
            new_start_page_token: Some("page-2".to_owned()),
        });
        let checkpoints = Arc::new(FakeCheckpoints::new(checkpoint()));
        let job = job(
            drive,
            checkpoints,
            Arc::new(FakeEvidence::default()),
            Arc::new(FakeBroker {
                fail_once: Mutex::new(false),
                output: None,
            }),
        );

        let report = job.run_once(RunMode::Apply).unwrap();
        assert_eq!(report.ignored, 1);
        assert!(report.unbound.is_empty());
    }

    fn initial_pdf(file_id: &str, parent: &str, bytes: &[u8]) -> DriveFileRevision {
        DriveFileRevision {
            file_id: file_id.to_owned(),
            name: "clean-original.pdf".to_owned(),
            version: 1,
            mime_type: "application/pdf".to_owned(),
            parents: vec![parent.to_owned()],
            size: bytes.len() as u64,
            trashed: false,
            app_properties: BTreeMap::new(),
        }
    }

    #[test]
    fn fresh_checkpoint_bootstraps_both_approved_originals_before_saving_cursor() {
        let bytes = b"%PDF-1.7 clean original";
        let boox = initial_pdf("boox-original", "boox-folder-123", bytes);
        let supernote = initial_pdf("supernote-original", "supernote-folder-123", bytes);
        let drive = Arc::new(FakeDrive::default());
        *drive.initial_files.lock().unwrap() = vec![boox.clone(), supernote.clone()];
        drive.downloads.lock().unwrap().extend([
            (boox.file_id.clone(), bytes.to_vec()),
            (supernote.file_id.clone(), bytes.to_vec()),
        ]);
        let approvals = Arc::new(FakeApprovals::default());
        for file in [&boox, &supernote] {
            approvals.values.lock().unwrap().insert(
                file.file_id.clone(),
                OnboardingApproval::Original {
                    approval: OriginalRegistrationApproval {
                        drive_file_id: file.file_id.clone(),
                        drive_file_version: file.version,
                        content_sha256: sha256_hex(bytes),
                    },
                },
            );
        }
        let checkpoints = Arc::new(FakeCheckpoints::new(DriveGatewayCheckpoint::empty()));
        let evidence = Arc::new(FakeEvidence::default());
        let job = job_with_approvals(
            drive,
            checkpoints.clone(),
            evidence.clone(),
            Arc::new(FakeBroker {
                fail_once: Mutex::new(false),
                output: None,
            }),
            approvals,
        );

        let report = job.run_once(RunMode::Apply).unwrap();
        assert!(report.initialized_page_token);
        assert_eq!(report.changes_seen, 2);
        let checkpoint = checkpoints.value();
        assert_eq!(checkpoint.next_page_token.as_deref(), Some("initial"));
        assert_eq!(checkpoint.documents.len(), 1);
        let document_id = inkbridge_broker::stable_document_id(bytes);
        let binding = &checkpoint.documents[&document_id];
        assert!(binding.boox_file_ids.contains("boox-original"));
        assert!(binding.supernote_file_ids.contains("supernote-original"));
        assert_eq!(evidence.objects.lock().unwrap().len(), 2);
    }

    #[test]
    fn first_dry_run_inspects_existing_files_without_mutating_state() {
        let bytes = b"%PDF-1.7 clean original";
        let file = initial_pdf("boox-original", "boox-folder-123", bytes);
        let drive = Arc::new(FakeDrive::default());
        *drive.initial_files.lock().unwrap() = vec![file.clone()];
        drive
            .downloads
            .lock()
            .unwrap()
            .insert(file.file_id.clone(), bytes.to_vec());
        let approvals = Arc::new(FakeApprovals::default());
        approvals.values.lock().unwrap().insert(
            file.file_id.clone(),
            OnboardingApproval::Original {
                approval: OriginalRegistrationApproval {
                    drive_file_id: file.file_id,
                    drive_file_version: file.version,
                    content_sha256: sha256_hex(bytes),
                },
            },
        );
        let checkpoints = Arc::new(FakeCheckpoints::new(DriveGatewayCheckpoint::empty()));
        let evidence = Arc::new(FakeEvidence::default());
        let job = job_with_approvals(
            drive,
            checkpoints.clone(),
            evidence.clone(),
            Arc::new(FakeBroker {
                fail_once: Mutex::new(false),
                output: None,
            }),
            approvals,
        );

        let report = job.run_once(RunMode::DryRun).unwrap();
        assert!(report.initialized_page_token);
        assert_eq!(report.changes_seen, 1);
        assert!(report
            .dry_run_actions
            .iter()
            .any(|action| action.contains("register approved original")));
        assert_eq!(checkpoints.value(), DriveGatewayCheckpoint::empty());
        assert!(evidence.objects.lock().unwrap().is_empty());
    }

    #[test]
    fn unapproved_initial_file_blocks_cursor_commit_in_apply_mode() {
        let bytes = b"%PDF-1.7 unknown PDF";
        let file = initial_pdf("unknown-file", "boox-folder-123", bytes);
        let drive = Arc::new(FakeDrive::default());
        *drive.initial_files.lock().unwrap() = vec![file.clone()];
        drive
            .downloads
            .lock()
            .unwrap()
            .insert(file.file_id, bytes.to_vec());
        let checkpoints = Arc::new(FakeCheckpoints::new(DriveGatewayCheckpoint::empty()));
        let job = job(
            drive,
            checkpoints.clone(),
            Arc::new(FakeEvidence::default()),
            Arc::new(FakeBroker {
                fail_once: Mutex::new(false),
                output: None,
            }),
        );

        let error = job.run_once(RunMode::Apply).unwrap_err();
        assert!(error.contains("no explicit onboarding approval"));
        assert!(checkpoints.value().next_page_token.is_none());
    }

    #[test]
    fn equal_sized_newer_drive_revision_is_rejected_after_download() {
        let bytes = br#"{"schemaVersion":1}"#;
        let drive = Arc::new(FakeDrive::with_page(bytes));
        let mut newer = change(bytes).file;
        newer.version += 1;
        drive
            .current_revisions
            .lock()
            .unwrap()
            .insert(newer.file_id.clone(), newer);
        let checkpoints = Arc::new(FakeCheckpoints::new(checkpoint()));
        let evidence = Arc::new(FakeEvidence::default());
        let job = job(
            drive,
            checkpoints.clone(),
            evidence.clone(),
            Arc::new(FakeBroker {
                fail_once: Mutex::new(false),
                output: None,
            }),
        );

        let error = job.run_once(RunMode::Apply).unwrap_err();
        assert!(error.contains("changed while it was being downloaded"));
        assert_eq!(
            checkpoints.value().next_page_token.as_deref(),
            Some("page-1")
        );
        assert!(evidence.objects.lock().unwrap().is_empty());
    }

    #[test]
    fn approved_unbound_device_artifact_is_bound_and_processed_on_the_same_page() {
        let bytes = br#"{"schemaVersion":1,"operations":[]}"#;
        let artifact = DriveFileRevision {
            file_id: "new-supernote-export".to_owned(),
            name: "native-export.json".to_owned(),
            version: 4,
            mime_type: "application/json".to_owned(),
            parents: vec!["supernote-folder-123".to_owned()],
            size: bytes.len() as u64,
            trashed: false,
            app_properties: BTreeMap::new(),
        };
        let drive = Arc::new(FakeDrive::default());
        *drive.page.lock().unwrap() = Some(DriveChangePage {
            changes: vec![DriveChange {
                file: artifact.clone(),
            }],
            next_page_token: None,
            new_start_page_token: Some("page-2".to_owned()),
        });
        drive
            .downloads
            .lock()
            .unwrap()
            .insert(artifact.file_id.clone(), bytes.to_vec());
        let approvals = Arc::new(FakeApprovals::default());
        approvals.values.lock().unwrap().insert(
            artifact.file_id.clone(),
            OnboardingApproval::DeviceArtifact {
                approval: DeviceArtifactBindingApproval {
                    drive_file_id: artifact.file_id.clone(),
                    drive_file_version: artifact.version,
                    content_sha256: sha256_hex(bytes),
                    document_id: document_id(),
                    source: inkbridge_broker::DeviceSide::Supernote,
                    based_on: RevisionPair::default(),
                },
            },
        );
        let checkpoints = Arc::new(FakeCheckpoints::new(checkpoint()));
        let job = job_with_approvals(
            drive,
            checkpoints.clone(),
            Arc::new(FakeEvidence::default()),
            Arc::new(FakeBroker {
                fail_once: Mutex::new(false),
                output: None,
            }),
            approvals,
        );

        let report = job.run_once(RunMode::Apply).unwrap();
        assert_eq!(report.inputs_applied, 1);
        let checkpoint = checkpoints.value();
        assert!(checkpoint.documents[&document_id()]
            .supernote_file_ids
            .contains("new-supernote-export"));
        assert_eq!(
            checkpoint.file_observed_frontiers["new-supernote-export"].supernote,
            1
        );
        assert_eq!(checkpoint.next_page_token.as_deref(), Some("page-2"));
    }

    #[test]
    fn bootstrap_registers_original_before_an_earlier_sorted_dependent_artifact() {
        let original_bytes = b"%PDF-1.7 clean original";
        let artifact_bytes = br#"{"schemaVersion":1,"operations":[]}"#;
        let original = initial_pdf("z-original", "boox-folder-123", original_bytes);
        let artifact = DriveFileRevision {
            file_id: "a-artifact".to_owned(),
            name: "native-export.json".to_owned(),
            version: 1,
            mime_type: "application/json".to_owned(),
            parents: vec!["supernote-folder-123".to_owned()],
            size: artifact_bytes.len() as u64,
            trashed: false,
            app_properties: BTreeMap::new(),
        };
        let document_id = inkbridge_broker::stable_document_id(original_bytes);
        let drive = Arc::new(FakeDrive::default());
        *drive.initial_files.lock().unwrap() = vec![artifact.clone(), original.clone()];
        drive.downloads.lock().unwrap().extend([
            (artifact.file_id.clone(), artifact_bytes.to_vec()),
            (original.file_id.clone(), original_bytes.to_vec()),
        ]);
        let approvals = Arc::new(FakeApprovals::default());
        approvals.values.lock().unwrap().extend([
            (
                original.file_id.clone(),
                OnboardingApproval::Original {
                    approval: OriginalRegistrationApproval {
                        drive_file_id: original.file_id.clone(),
                        drive_file_version: original.version,
                        content_sha256: sha256_hex(original_bytes),
                    },
                },
            ),
            (
                artifact.file_id.clone(),
                OnboardingApproval::DeviceArtifact {
                    approval: DeviceArtifactBindingApproval {
                        drive_file_id: artifact.file_id.clone(),
                        drive_file_version: artifact.version,
                        content_sha256: sha256_hex(artifact_bytes),
                        document_id: document_id.clone(),
                        source: inkbridge_broker::DeviceSide::Supernote,
                        based_on: RevisionPair::default(),
                    },
                },
            ),
        ]);
        let checkpoints = Arc::new(FakeCheckpoints::new(DriveGatewayCheckpoint::empty()));
        let broker = Arc::new(OrderingBroker {
            registered: Mutex::new(BTreeSet::new()),
        });
        let job = GatewayJob::new(
            config(),
            drive,
            checkpoints.clone(),
            Arc::new(FakeEvidence::default()),
            broker.clone(),
            approvals,
        )
        .unwrap();

        let report = job.run_once(RunMode::Apply).unwrap();
        assert_eq!(report.inputs_applied, 1);
        assert!(broker.registered.lock().unwrap().contains(&document_id));
        let checkpoint = checkpoints.value();
        assert!(checkpoint.documents[&document_id]
            .boox_file_ids
            .contains("z-original"));
        assert!(checkpoint.documents[&document_id]
            .supernote_file_ids
            .contains("a-artifact"));
        assert_eq!(checkpoint.next_page_token.as_deref(), Some("initial"));
    }

    #[test]
    fn bootstrap_dry_run_simulates_original_before_validating_dependent_artifact() {
        let original_bytes = b"%PDF-1.7 clean original";
        let artifact_bytes = br#"{"schemaVersion":1,"operations":[]}"#;
        let original = initial_pdf("z-original", "boox-folder-123", original_bytes);
        let artifact = DriveFileRevision {
            file_id: "a-artifact".to_owned(),
            name: "native-export.json".to_owned(),
            version: 1,
            mime_type: "application/json".to_owned(),
            parents: vec!["supernote-folder-123".to_owned()],
            size: artifact_bytes.len() as u64,
            trashed: false,
            app_properties: BTreeMap::new(),
        };
        let document_id = inkbridge_broker::stable_document_id(original_bytes);
        let drive = Arc::new(FakeDrive::default());
        *drive.initial_files.lock().unwrap() = vec![artifact.clone(), original.clone()];
        drive.downloads.lock().unwrap().extend([
            (artifact.file_id.clone(), artifact_bytes.to_vec()),
            (original.file_id.clone(), original_bytes.to_vec()),
        ]);
        let approvals = Arc::new(FakeApprovals::default());
        approvals.values.lock().unwrap().extend([
            (
                original.file_id.clone(),
                OnboardingApproval::Original {
                    approval: OriginalRegistrationApproval {
                        drive_file_id: original.file_id.clone(),
                        drive_file_version: original.version,
                        content_sha256: sha256_hex(original_bytes),
                    },
                },
            ),
            (
                artifact.file_id.clone(),
                OnboardingApproval::DeviceArtifact {
                    approval: DeviceArtifactBindingApproval {
                        drive_file_id: artifact.file_id.clone(),
                        drive_file_version: artifact.version,
                        content_sha256: sha256_hex(artifact_bytes),
                        document_id: document_id.clone(),
                        source: inkbridge_broker::DeviceSide::Supernote,
                        based_on: RevisionPair::default(),
                    },
                },
            ),
        ]);
        let checkpoints = Arc::new(FakeCheckpoints::new(DriveGatewayCheckpoint::empty()));
        let evidence = Arc::new(FakeEvidence::default());
        let broker = Arc::new(OrderingBroker {
            registered: Mutex::new(BTreeSet::new()),
        });
        let job = GatewayJob::new(
            config(),
            drive,
            checkpoints.clone(),
            evidence.clone(),
            broker.clone(),
            approvals,
        )
        .unwrap();

        let report = job.run_once(RunMode::DryRun).unwrap();

        assert!(report
            .dry_run_actions
            .iter()
            .any(|action| action.contains("register approved original")));
        assert!(report
            .dry_run_actions
            .iter()
            .any(|action| action.contains("bind approved device artifact")));
        assert!(report
            .dry_run_actions
            .iter()
            .any(|action| action.contains("upload drive-v1-")));
        assert!(broker.registered.lock().unwrap().is_empty());
        assert!(evidence.objects.lock().unwrap().is_empty());
        let checkpoint = checkpoints.value();
        assert!(checkpoint.documents.is_empty());
        assert!(checkpoint.next_page_token.is_none());
    }

    #[test]
    fn broker_rejection_never_commits_an_original_binding_or_cursor() {
        let bytes = b"not actually a PDF";
        let file = initial_pdf("bad-original", "boox-folder-123", bytes);
        let drive = Arc::new(FakeDrive::default());
        *drive.initial_files.lock().unwrap() = vec![file.clone()];
        drive
            .downloads
            .lock()
            .unwrap()
            .insert(file.file_id.clone(), bytes.to_vec());
        let approvals = Arc::new(FakeApprovals::default());
        approvals.values.lock().unwrap().insert(
            file.file_id.clone(),
            OnboardingApproval::Original {
                approval: OriginalRegistrationApproval {
                    drive_file_id: file.file_id,
                    drive_file_version: file.version,
                    content_sha256: sha256_hex(bytes),
                },
            },
        );
        let checkpoints = Arc::new(FakeCheckpoints::new(DriveGatewayCheckpoint::empty()));
        let evidence = Arc::new(FakeEvidence::default());
        let job = GatewayJob::new(
            config(),
            drive,
            checkpoints.clone(),
            evidence.clone(),
            Arc::new(RejectingRegistrationBroker),
            approvals,
        )
        .unwrap();

        let error = job.run_once(RunMode::Apply).unwrap_err();
        assert!(error.contains("rejected malformed original PDF"));
        let checkpoint = checkpoints.value();
        assert!(checkpoint.documents.is_empty());
        assert!(checkpoint.next_page_token.is_none());
        assert_eq!(evidence.objects.lock().unwrap().len(), 1);
    }
}
