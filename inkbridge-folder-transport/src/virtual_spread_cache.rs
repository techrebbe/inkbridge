use inkbridge_convert::VirtualSpreadProductionVerification;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(any(target_os = "linux", target_os = "android"))]
use std::fs::File;
#[cfg(any(target_os = "linux", target_os = "android"))]
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const VIRTUAL_SPREAD_CACHE_TRANSACTION_SCHEMA_VERSION: u32 = 2;
pub const VIRTUAL_SPREAD_CACHE_RELATIVE_ROOT: &str = ".inkbridge/virtual-spread/v1";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VirtualSpreadViewEvidence {
    pub document_id: String,
    pub view_id: String,
    pub cache_basename: String,
    pub generated_pdf_sha256: String,
    pub manifest_sha256: String,
    pub mapping_authority_sha256: String,
}

impl VirtualSpreadViewEvidence {
    pub fn from_production_verification(
        verification: &VirtualSpreadProductionVerification,
    ) -> Self {
        let manifest = verification.manifest();
        Self {
            document_id: manifest.document_id.clone(),
            view_id: manifest.view_id.clone(),
            cache_basename: manifest.cache_basename.clone(),
            generated_pdf_sha256: verification.generated_pdf_sha256().to_owned(),
            manifest_sha256: verification.sidecar_sha256().to_owned(),
            mapping_authority_sha256: manifest.mapping_authority_sha256.clone(),
        }
    }
}

#[derive(Debug)]
pub struct MaterializedVirtualSpreadCache {
    pub directory: PathBuf,
    pub pdf_path: PathBuf,
    pub manifest_path: PathBuf,
    pub nomedia_path: PathBuf,
    verified_pdf_bytes: Vec<u8>,
    verified_manifest_bytes: Vec<u8>,
}

impl MaterializedVirtualSpreadCache {
    /// Consume the materialization result and transfer the exact verified bytes
    /// to activation. These are the same owned buffers used for publication,
    /// not a second large-PDF copy and not a reread of mutable shared storage.
    pub fn into_verified_activation(self) -> VerifiedVirtualSpreadActivation {
        VerifiedVirtualSpreadActivation {
            pdf_bytes: self.verified_pdf_bytes,
            manifest_bytes: self.verified_manifest_bytes,
        }
    }
}

impl PartialEq for MaterializedVirtualSpreadCache {
    fn eq(&self, other: &Self) -> bool {
        self.directory == other.directory
            && self.pdf_path == other.pdf_path
            && self.manifest_path == other.manifest_path
            && self.nomedia_path == other.nomedia_path
    }
}

impl Eq for MaterializedVirtualSpreadCache {}

#[derive(Debug)]
pub struct VerifiedVirtualSpreadActivation {
    pdf_bytes: Vec<u8>,
    manifest_bytes: Vec<u8>,
}

impl VerifiedVirtualSpreadActivation {
    pub fn pdf_bytes(&self) -> &[u8] {
        &self.pdf_bytes
    }

    pub fn manifest_bytes(&self) -> &[u8] {
        &self.manifest_bytes
    }
}

/// Publish a verified PDF/sidecar pair under the authenticated hidden-cache name.
///
/// `shared_storage_root` is the device's shared-storage root (for example,
/// `/storage/emulated/0`). The complete PDF/sidecar directory is published in one
/// atomic rename, so a crash cannot expose a mixed or incomplete pair. Existing
/// identical bytes are accepted idempotently; different bytes at the versioned
/// path fail closed. Publication is supported only on Linux/Android, where the
/// required no-replace directory rename, handle-relative no-follow operations,
/// advisory process lock, and durability barriers are available. The returned
/// owned input buffers become the immutable activation handoff without another
/// large-PDF copy; the shared-storage paths are locators and must not be treated
/// as immutable evidence. Other hosts fail closed rather than approximating the
/// device-cache safety contract.
pub fn materialize_virtual_spread_cache(
    shared_storage_root: &Path,
    verification: &VirtualSpreadProductionVerification,
    generated_pdf: Vec<u8>,
    manifest: Vec<u8>,
) -> Result<MaterializedVirtualSpreadCache, String> {
    let view = VirtualSpreadViewEvidence::from_production_verification(verification);
    validate_view(&view, &view.document_id)?;
    if sha256_hex(&generated_pdf) != view.generated_pdf_sha256 {
        return Err("generated PDF bytes do not match Virtual Spread view evidence".to_owned());
    }
    if sha256_hex(&manifest) != view.manifest_sha256 {
        return Err("manifest bytes do not match Virtual Spread view evidence".to_owned());
    }
    let directory = shared_storage_root
        .join(VIRTUAL_SPREAD_CACHE_RELATIVE_ROOT)
        .join(&view.document_id)
        .join(&view.view_id);
    let nomedia_path = directory.join(".nomedia");
    let pdf_path = directory.join(&view.cache_basename);
    let manifest_path = directory.join(format!("{}.json", view.cache_basename));
    publish_cache_files(shared_storage_root, &view, &generated_pdf, &manifest)?;

    Ok(MaterializedVirtualSpreadCache {
        directory,
        pdf_path,
        manifest_path,
        nomedia_path,
        verified_pdf_bytes: generated_pdf,
        verified_manifest_bytes: manifest,
    })
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirtyMarkExportEvidence {
    pub active_view_id: String,
    pub exported_snapshot_sha256: String,
    pub canonical_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HydrationEvidence {
    pub candidate_view_id: String,
    pub canonical_revision: u64,
    pub represented_source_pages: Vec<u32>,
    pub hydrated_mark_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CandidateVerificationEvidence {
    pub candidate_view_id: String,
    pub generated_pdf_sha256: String,
    pub manifest_sha256: String,
    pub mapping_authority_sha256: String,
    pub hydrated_mark_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RollbackEvidence {
    pub previous_mapping_sha256: String,
    pub previous_view_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheRegenerationPhase {
    AwaitingDirtyExport,
    AwaitingGeneration,
    AwaitingHydration,
    AwaitingVerification,
    ReadyToActivate,
    Activated,
    RolledBack,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheRegenerationMode {
    Clean,
    Dirty,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CacheRegenerationTransaction {
    pub schema_version: u32,
    pub transaction_id: String,
    pub document_id: String,
    pub original_pdf_sha256: String,
    pub represented_source_pages: Vec<u32>,
    pub previous_view: Option<VirtualSpreadViewEvidence>,
    pub candidate_view: VirtualSpreadViewEvidence,
    pub mode: CacheRegenerationMode,
    pub phase: CacheRegenerationPhase,
    pub canonical_revision: Option<u64>,
    pub dirty_export: Option<DirtyMarkExportEvidence>,
    pub hydration: Option<HydrationEvidence>,
    pub verification: Option<CandidateVerificationEvidence>,
    pub rollback: Option<RollbackEvidence>,
    pub active_mapping_sha256: Option<String>,
}

impl CacheRegenerationTransaction {
    pub fn begin_clean(
        transaction_id: String,
        original_pdf_sha256: String,
        canonical_revision: u64,
        represented_source_pages: Vec<u32>,
        previous_view: Option<VirtualSpreadViewEvidence>,
        candidate_view: VirtualSpreadViewEvidence,
    ) -> Result<Self, String> {
        Self::begin(
            transaction_id,
            original_pdf_sha256,
            represented_source_pages,
            previous_view,
            candidate_view,
            CacheRegenerationMode::Clean,
            Some(canonical_revision),
        )
    }

    pub fn begin_dirty(
        transaction_id: String,
        original_pdf_sha256: String,
        represented_source_pages: Vec<u32>,
        previous_view: VirtualSpreadViewEvidence,
        candidate_view: VirtualSpreadViewEvidence,
    ) -> Result<Self, String> {
        Self::begin(
            transaction_id,
            original_pdf_sha256,
            represented_source_pages,
            Some(previous_view),
            candidate_view,
            CacheRegenerationMode::Dirty,
            None,
        )
    }

    fn begin(
        transaction_id: String,
        original_pdf_sha256: String,
        represented_source_pages: Vec<u32>,
        previous_view: Option<VirtualSpreadViewEvidence>,
        candidate_view: VirtualSpreadViewEvidence,
        mode: CacheRegenerationMode,
        canonical_revision: Option<u64>,
    ) -> Result<Self, String> {
        if transaction_id.trim().is_empty() {
            return Err("Virtual Spread cache transaction ID is empty".to_owned());
        }
        validate_sha256(&original_pdf_sha256, "original PDF SHA-256")?;
        let document_id = format!("inkbridge-doc-v1-{original_pdf_sha256}");
        validate_pages(&represented_source_pages)?;
        validate_view(&candidate_view, &document_id)?;
        if let Some(previous) = &previous_view {
            validate_view(previous, &document_id)?;
            validate_versioned_candidate(previous, &candidate_view)?;
        }
        let phase = if canonical_revision.is_some() {
            CacheRegenerationPhase::AwaitingGeneration
        } else {
            CacheRegenerationPhase::AwaitingDirtyExport
        };
        Ok(Self {
            schema_version: VIRTUAL_SPREAD_CACHE_TRANSACTION_SCHEMA_VERSION,
            transaction_id,
            document_id,
            original_pdf_sha256,
            represented_source_pages,
            previous_view,
            candidate_view,
            mode,
            phase,
            canonical_revision,
            dirty_export: None,
            hydration: None,
            verification: None,
            rollback: None,
            active_mapping_sha256: None,
        })
    }

    pub fn record_dirty_export(&mut self, evidence: DirtyMarkExportEvidence) -> Result<(), String> {
        self.require_phase(CacheRegenerationPhase::AwaitingDirtyExport)?;
        let previous = self
            .previous_view
            .as_ref()
            .ok_or_else(|| "dirty cache regeneration has no previous view".to_owned())?;
        if evidence.active_view_id != previous.view_id {
            return Err("dirty .mark export does not belong to the active view".to_owned());
        }
        validate_sha256(
            &evidence.exported_snapshot_sha256,
            "dirty export snapshot SHA-256",
        )?;
        self.canonical_revision = Some(evidence.canonical_revision);
        self.dirty_export = Some(evidence);
        self.phase = CacheRegenerationPhase::AwaitingGeneration;
        Ok(())
    }

    pub fn record_generated(&mut self, evidence: &VirtualSpreadViewEvidence) -> Result<(), String> {
        self.require_phase(CacheRegenerationPhase::AwaitingGeneration)?;
        if evidence != &self.candidate_view {
            return Err(
                "generated Virtual Spread evidence does not match the candidate view".to_owned(),
            );
        }
        self.phase = CacheRegenerationPhase::AwaitingHydration;
        Ok(())
    }

    pub fn record_hydration(&mut self, evidence: HydrationEvidence) -> Result<(), String> {
        self.require_phase(CacheRegenerationPhase::AwaitingHydration)?;
        if evidence.candidate_view_id != self.candidate_view.view_id
            || Some(evidence.canonical_revision) != self.canonical_revision
            || evidence.represented_source_pages != self.represented_source_pages
        {
            return Err(
                "Virtual Spread hydration is not one complete canonical revision for every represented page"
                    .to_owned(),
            );
        }
        validate_sha256(&evidence.hydrated_mark_sha256, "hydrated .mark SHA-256")?;
        self.hydration = Some(evidence);
        self.phase = CacheRegenerationPhase::AwaitingVerification;
        Ok(())
    }

    pub fn record_verification(
        &mut self,
        evidence: CandidateVerificationEvidence,
        rollback: RollbackEvidence,
    ) -> Result<(), String> {
        self.require_phase(CacheRegenerationPhase::AwaitingVerification)?;
        let hydration = self
            .hydration
            .as_ref()
            .ok_or_else(|| "Virtual Spread hydration evidence is missing".to_owned())?;
        if evidence.candidate_view_id != self.candidate_view.view_id
            || evidence.generated_pdf_sha256 != self.candidate_view.generated_pdf_sha256
            || evidence.manifest_sha256 != self.candidate_view.manifest_sha256
            || evidence.mapping_authority_sha256 != self.candidate_view.mapping_authority_sha256
            || evidence.hydrated_mark_sha256 != hydration.hydrated_mark_sha256
        {
            return Err(
                "verified Virtual Spread candidate does not match generated/hydrated evidence"
                    .to_owned(),
            );
        }
        validate_rollback(&rollback, self.previous_view.as_ref())?;
        self.verification = Some(evidence);
        self.rollback = Some(rollback);
        self.phase = CacheRegenerationPhase::ReadyToActivate;
        Ok(())
    }

    pub fn commit_activation(&mut self, active_mapping_sha256: String) -> Result<(), String> {
        self.require_phase(CacheRegenerationPhase::ReadyToActivate)?;
        validate_sha256(&active_mapping_sha256, "active mapping SHA-256")?;
        if self.rollback.is_none() || self.verification.is_none() {
            return Err(
                "Virtual Spread activation lacks verification or rollback evidence".to_owned(),
            );
        }
        self.active_mapping_sha256 = Some(active_mapping_sha256);
        self.phase = CacheRegenerationPhase::Activated;
        Ok(())
    }

    pub fn rollback(&mut self) -> Result<(), String> {
        if matches!(
            self.phase,
            CacheRegenerationPhase::Activated | CacheRegenerationPhase::RolledBack
        ) {
            return Err(
                "completed Virtual Spread cache transaction cannot be rolled back in place"
                    .to_owned(),
            );
        }
        self.phase = CacheRegenerationPhase::RolledBack;
        Ok(())
    }

    pub fn validate_persisted(&self) -> Result<(), String> {
        if self.schema_version != VIRTUAL_SPREAD_CACHE_TRANSACTION_SCHEMA_VERSION {
            return Err("unsupported Virtual Spread cache transaction schema".to_owned());
        }
        if self.transaction_id.trim().is_empty() {
            return Err("Virtual Spread cache transaction ID is empty".to_owned());
        }
        validate_sha256(&self.original_pdf_sha256, "original PDF SHA-256")?;
        if self.document_id != format!("inkbridge-doc-v1-{}", self.original_pdf_sha256) {
            return Err("cache transaction document ID does not match original PDF".to_owned());
        }
        validate_pages(&self.represented_source_pages)?;
        validate_view(&self.candidate_view, &self.document_id)?;
        if let Some(previous) = &self.previous_view {
            validate_view(previous, &self.document_id)?;
            validate_versioned_candidate(previous, &self.candidate_view)?;
        }
        if let Some(export) = &self.dirty_export {
            let previous = self
                .previous_view
                .as_ref()
                .ok_or_else(|| "dirty export evidence has no previous view".to_owned())?;
            if export.active_view_id != previous.view_id
                || Some(export.canonical_revision) != self.canonical_revision
            {
                return Err(
                    "dirty export evidence is not bound to the previous view/revision".to_owned(),
                );
            }
            validate_sha256(
                &export.exported_snapshot_sha256,
                "dirty export snapshot SHA-256",
            )?;
        }
        if let Some(hydration) = &self.hydration {
            if hydration.candidate_view_id != self.candidate_view.view_id
                || Some(hydration.canonical_revision) != self.canonical_revision
                || hydration.represented_source_pages != self.represented_source_pages
            {
                return Err("persisted hydration evidence is not transaction-bound".to_owned());
            }
            validate_sha256(&hydration.hydrated_mark_sha256, "hydrated .mark SHA-256")?;
        }
        if let Some(verification) = &self.verification {
            let hydration = self
                .hydration
                .as_ref()
                .ok_or_else(|| "verification evidence has no hydration evidence".to_owned())?;
            if verification.candidate_view_id != self.candidate_view.view_id
                || verification.generated_pdf_sha256 != self.candidate_view.generated_pdf_sha256
                || verification.manifest_sha256 != self.candidate_view.manifest_sha256
                || verification.mapping_authority_sha256
                    != self.candidate_view.mapping_authority_sha256
                || verification.hydrated_mark_sha256 != hydration.hydrated_mark_sha256
            {
                return Err("persisted candidate verification evidence is inconsistent".to_owned());
            }
        }
        if let Some(rollback) = &self.rollback {
            validate_rollback(rollback, self.previous_view.as_ref())?;
        }
        if let Some(active_mapping_sha256) = &self.active_mapping_sha256 {
            validate_sha256(active_mapping_sha256, "active mapping SHA-256")?;
        }
        match self.mode {
            CacheRegenerationMode::Clean => {
                if self.canonical_revision.is_none()
                    || self.dirty_export.is_some()
                    || self.phase == CacheRegenerationPhase::AwaitingDirtyExport
                {
                    return Err("invalid clean cache regeneration mode/evidence".to_owned());
                }
            }
            CacheRegenerationMode::Dirty => {
                if self.previous_view.is_none() {
                    return Err("dirty cache regeneration has no previous view".to_owned());
                }
                match self.phase {
                    CacheRegenerationPhase::AwaitingDirtyExport => {}
                    CacheRegenerationPhase::RolledBack => {
                        if self.canonical_revision.is_some() != self.dirty_export.is_some() {
                            return Err(
                                "rolled-back dirty regeneration has incomplete export evidence"
                                    .to_owned(),
                            );
                        }
                    }
                    _ => {
                        if self.canonical_revision.is_none() || self.dirty_export.is_none() {
                            return Err(
                                "dirty cache regeneration is missing required export evidence"
                                    .to_owned(),
                            );
                        }
                    }
                }
            }
        }
        match self.phase {
            CacheRegenerationPhase::AwaitingDirtyExport => {
                if self.previous_view.is_none()
                    || self.canonical_revision.is_some()
                    || self.dirty_export.is_some()
                    || self.hydration.is_some()
                    || self.verification.is_some()
                    || self.rollback.is_some()
                    || self.active_mapping_sha256.is_some()
                {
                    return Err("invalid awaiting-dirty-export transaction state".to_owned());
                }
            }
            CacheRegenerationPhase::AwaitingGeneration => {
                if self.canonical_revision.is_none()
                    || self.hydration.is_some()
                    || self.verification.is_some()
                    || self.rollback.is_some()
                    || self.active_mapping_sha256.is_some()
                {
                    return Err("generation is not bound to a canonical revision".to_owned());
                }
            }
            CacheRegenerationPhase::AwaitingHydration => {
                if self.canonical_revision.is_none()
                    || self.hydration.is_some()
                    || self.verification.is_some()
                    || self.rollback.is_some()
                    || self.active_mapping_sha256.is_some()
                {
                    return Err(
                        "generated candidate is not bound to a canonical revision".to_owned()
                    );
                }
            }
            CacheRegenerationPhase::AwaitingVerification => {
                if self.hydration.is_none()
                    || self.verification.is_some()
                    || self.rollback.is_some()
                    || self.active_mapping_sha256.is_some()
                {
                    return Err(
                        "candidate awaiting verification lacks hydration evidence".to_owned()
                    );
                }
            }
            CacheRegenerationPhase::ReadyToActivate => {
                if self.verification.is_none()
                    || self.rollback.is_none()
                    || self.active_mapping_sha256.is_some()
                {
                    return Err("candidate ready to activate lacks durable evidence".to_owned());
                }
            }
            CacheRegenerationPhase::Activated => {
                if self.verification.is_none()
                    || self.rollback.is_none()
                    || self.active_mapping_sha256.is_none()
                {
                    return Err("activated candidate lacks durable activation evidence".to_owned());
                }
            }
            CacheRegenerationPhase::RolledBack => {
                if self.active_mapping_sha256.is_some() {
                    return Err(
                        "rolled-back Virtual Spread transaction retains activation evidence"
                            .to_owned(),
                    );
                }
            }
        }
        Ok(())
    }

    fn require_phase(&self, expected: CacheRegenerationPhase) -> Result<(), String> {
        if self.phase != expected {
            return Err(format!(
                "Virtual Spread cache transaction is {:?}, expected {:?}",
                self.phase, expected
            ));
        }
        Ok(())
    }
}

fn validate_versioned_candidate(
    previous: &VirtualSpreadViewEvidence,
    candidate: &VirtualSpreadViewEvidence,
) -> Result<(), String> {
    if previous.view_id == candidate.view_id || previous.cache_basename == candidate.cache_basename
    {
        return Err(
            "Virtual Spread regeneration requires a new versioned candidate view".to_owned(),
        );
    }
    Ok(())
}

fn validate_view(view: &VirtualSpreadViewEvidence, document_id: &str) -> Result<(), String> {
    if view.document_id != document_id {
        return Err("Virtual Spread view belongs to a different original document".to_owned());
    }
    let Some(document_hash) = view.document_id.strip_prefix("inkbridge-doc-v1-") else {
        return Err("Virtual Spread document ID has an unsupported prefix".to_owned());
    };
    validate_sha256(document_hash, "Virtual Spread document hash")?;
    let Some(view_hash) = view.view_id.strip_prefix("inkbridge-view-v1-") else {
        return Err("Virtual Spread view ID has an unsupported prefix".to_owned());
    };
    validate_sha256(view_hash, "Virtual Spread view hash")?;
    if view.cache_basename != format!("{document_id}.{}.virtual-spread.pdf", view.view_id) {
        return Err("Virtual Spread cache basename is not document/view derived".to_owned());
    }
    validate_sha256(&view.generated_pdf_sha256, "generated PDF SHA-256")?;
    validate_sha256(&view.manifest_sha256, "manifest SHA-256")?;
    validate_sha256(&view.mapping_authority_sha256, "mapping authority SHA-256")?;
    Ok(())
}

fn validate_pages(pages: &[u32]) -> Result<(), String> {
    if pages.is_empty()
        || pages.iter().any(|page| *page > i32::MAX as u32)
        || !pages.windows(2).all(|pair| pair[0] < pair[1])
    {
        return Err(
            "Virtual Spread represented pages must be nonempty, unique, sorted int32 indices"
                .to_owned(),
        );
    }
    Ok(())
}

fn validate_rollback(
    rollback: &RollbackEvidence,
    previous: Option<&VirtualSpreadViewEvidence>,
) -> Result<(), String> {
    validate_sha256(
        &rollback.previous_mapping_sha256,
        "rollback mapping SHA-256",
    )?;
    if rollback.previous_view_id.as_deref() != previous.map(|view| view.view_id.as_str()) {
        return Err("rollback evidence does not identify the previous active view".to_owned());
    }
    Ok(())
}

fn validate_sha256(value: &str, label: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(format!("{label} is not lowercase SHA-256"));
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn publish_cache_files(
    shared_storage_root: &Path,
    view: &VirtualSpreadViewEvidence,
    generated_pdf: &[u8],
    manifest: &[u8],
) -> Result<(), String> {
    use rustix::fs::{flock, open, openat, FlockOperation, Mode, OFlags};

    let mut document_directory = open(
        shared_storage_root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| {
        format!(
            "could not pin Virtual Spread shared-storage root {}: {error}",
            shared_storage_root.display()
        )
    })?;
    for component in [
        ".inkbridge",
        "virtual-spread",
        "v1",
        view.document_id.as_str(),
    ] {
        document_directory = ensure_child_directory_at(&document_directory, component)?;
    }

    // Serialize publishers for this immutable versioned view. The lock file is
    // intentionally persistent and tiny: process death releases the kernel lock,
    // allowing the next run to recover the deterministic staging directory.
    let lock_name = format!(".{}.publish.lock", view.view_id);
    let lock = openat(
        &document_directory,
        lock_name.as_str(),
        OFlags::RDWR | OFlags::CREATE | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(|error| format!("could not open Virtual Spread publication lock: {error}"))?;
    let lock_file = File::from(lock);
    let lock_metadata = lock_file
        .metadata()
        .map_err(|error| format!("could not inspect Virtual Spread publication lock: {error}"))?;
    if !lock_metadata.is_file() {
        return Err("Virtual Spread publication lock is not a regular file".to_owned());
    }
    let lock = std::os::fd::OwnedFd::from(lock_file);
    flock(&lock, FlockOperation::LockExclusive)
        .map_err(|error| format!("could not acquire Virtual Spread publication lock: {error}"))?;
    verify_leaf_is_still_attached(&document_directory, &lock_name, &lock, "publication lock")?;

    let staging_name = format!(".{}.part", view.view_id);
    remove_abandoned_staging_directory(&document_directory, &staging_name, view)?;
    if let Some(directory) = open_cache_directory_if_present(&document_directory, view)? {
        verify_complete_cache_directory(&directory, view, generated_pdf, manifest)?;
        // A previous process may have completed the no-replace rename and then
        // lost power before syncing this parent. The idempotent accepter must
        // provide that missing durability barrier before returning activation.
        rustix::fs::fsync(&document_directory).map_err(|error| {
            format!("could not durably accept existing Virtual Spread cache directory: {error}")
        })?;
        verify_directory_is_still_attached(shared_storage_root, view, &directory)?;
        return Ok(());
    }

    let staging_directory = create_staging_directory(&document_directory, &staging_name)?;
    let manifest_name = format!("{}.json", view.cache_basename);
    let stage_result = (|| {
        write_new_file_at(&staging_directory, ".nomedia", b"", "media-index marker")?;
        write_new_file_at(
            &staging_directory,
            &view.cache_basename,
            generated_pdf,
            "generated PDF",
        )?;
        write_new_file_at(
            &staging_directory,
            &manifest_name,
            manifest,
            "schema-v3 sidecar",
        )?;
        sync_directory_at(&staging_directory)?;
        publish_staging_directory(&document_directory, &staging_name, &view.view_id)?;
        rustix::fs::fsync(&document_directory).map_err(|error| {
            format!("could not durably publish Virtual Spread cache directory: {error}")
        })?;
        verify_complete_cache_directory(&staging_directory, view, generated_pdf, manifest)?;
        verify_directory_is_still_attached(shared_storage_root, view, &staging_directory)
    })();

    if stage_result.is_err() {
        // Best-effort cleanup is safe because this process holds the per-view
        // lock. Preserve the original publication error if cleanup also fails.
        let _ = remove_abandoned_staging_directory(&document_directory, &staging_name, view);
    }
    stage_result
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn open_cache_directory_if_present(
    parent: &std::os::fd::OwnedFd,
    view: &VirtualSpreadViewEvidence,
) -> Result<Option<std::os::fd::OwnedFd>, String> {
    use rustix::fs::{openat, Mode, OFlags};
    use rustix::io::Errno;

    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    match openat(parent, view.view_id.as_str(), flags, Mode::empty()) {
        Ok(directory) => Ok(Some(directory)),
        Err(Errno::NOENT) => Ok(None),
        Err(error) => Err(format!(
            "Virtual Spread cache view {} is not a real directory: {error}",
            view.view_id
        )),
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn create_staging_directory(
    parent: &std::os::fd::OwnedFd,
    staging_name: &str,
) -> Result<std::os::fd::OwnedFd, String> {
    use rustix::fs::{mkdirat, openat, Mode, OFlags};

    mkdirat(parent, staging_name, Mode::RWXU | Mode::RGRP | Mode::XGRP)
        .map_err(|error| format!("could not create Virtual Spread staging directory: {error}"))?;
    openat(
        parent,
        staging_name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| format!("could not pin Virtual Spread staging directory: {error}"))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn write_new_file_at(
    directory: &std::os::fd::OwnedFd,
    name: &str,
    bytes: &[u8],
    label: &str,
) -> Result<(), String> {
    use rustix::fs::{openat, Mode, OFlags};

    let file = openat(
        directory,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(|error| format!("could not stage Virtual Spread {label}: {error}"))?;
    let mut file = File::from(file);
    file.write_all(bytes)
        .and_then(|_| file.flush())
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("could not finalize staged Virtual Spread {label}: {error}"))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn publish_staging_directory(
    parent: &std::os::fd::OwnedFd,
    staging_name: &str,
    final_name: &str,
) -> Result<(), String> {
    use rustix::fs::{renameat_with, RenameFlags};

    renameat_with(
        parent,
        staging_name,
        parent,
        final_name,
        RenameFlags::NOREPLACE,
    )
    .map_err(|error| {
        format!("could not atomically publish complete Virtual Spread cache directory: {error}")
    })
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn remove_abandoned_staging_directory(
    parent: &std::os::fd::OwnedFd,
    staging_name: &str,
    view: &VirtualSpreadViewEvidence,
) -> Result<(), String> {
    use rustix::fs::{openat, unlinkat, AtFlags, Mode, OFlags};
    use rustix::io::Errno;

    let staging = match openat(
        parent,
        staging_name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(directory) => directory,
        Err(Errno::NOENT) => return Ok(()),
        Err(error) => {
            return Err(format!(
                "Virtual Spread staging path is not a real directory: {error}"
            ))
        }
    };
    for name in [
        ".nomedia".to_owned(),
        view.cache_basename.clone(),
        format!("{}.json", view.cache_basename),
    ] {
        match unlinkat(&staging, name.as_str(), AtFlags::empty()) {
            Ok(()) | Err(Errno::NOENT) => {}
            Err(error) => {
                return Err(format!(
                    "could not remove abandoned Virtual Spread staging file {name}: {error}"
                ))
            }
        }
    }
    unlinkat(parent, staging_name, AtFlags::REMOVEDIR).map_err(|error| {
        format!("could not remove abandoned Virtual Spread staging directory: {error}")
    })?;
    rustix::fs::fsync(parent).map_err(|error| {
        format!("could not durably remove abandoned Virtual Spread staging directory: {error}")
    })
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn verify_complete_cache_directory(
    directory: &std::os::fd::OwnedFd,
    view: &VirtualSpreadViewEvidence,
    generated_pdf: &[u8],
    manifest: &[u8],
) -> Result<(), String> {
    let manifest_name = format!("{}.json", view.cache_basename);
    verify_installed_bytes_at(directory, ".nomedia", b"", "media-index marker")?;
    verify_installed_bytes_at(
        directory,
        &view.cache_basename,
        generated_pdf,
        "generated PDF",
    )?;
    verify_installed_bytes_at(directory, &manifest_name, manifest, "schema-v3 sidecar")?;
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn verify_directory_is_still_attached(
    shared_storage_root: &Path,
    view: &VirtualSpreadViewEvidence,
    pinned: &std::os::fd::OwnedFd,
) -> Result<(), String> {
    use rustix::fs::{fstat, open, openat, Mode, OFlags};

    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut resolved = open(shared_storage_root, flags, Mode::empty()).map_err(|error| {
        format!(
            "Virtual Spread cache root changed during publication {}: {error}",
            shared_storage_root.display()
        )
    })?;
    for component in [
        ".inkbridge",
        "virtual-spread",
        "v1",
        view.document_id.as_str(),
        view.view_id.as_str(),
    ] {
        resolved = openat(&resolved, component, flags, Mode::empty()).map_err(|error| {
            format!("Virtual Spread cache path detached during publication at {component}: {error}")
        })?;
    }
    let expected = fstat(pinned)
        .map_err(|error| format!("could not identify pinned Virtual Spread cache: {error}"))?;
    let actual = fstat(&resolved)
        .map_err(|error| format!("could not identify resolved Virtual Spread cache: {error}"))?;
    if expected.st_dev != actual.st_dev || expected.st_ino != actual.st_ino {
        return Err("Virtual Spread cache path detached during publication".to_owned());
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn ensure_child_directory_at(
    parent: &std::os::fd::OwnedFd,
    child: &str,
) -> Result<std::os::fd::OwnedFd, String> {
    use rustix::fs::{fsync, mkdirat, openat, Mode, OFlags};
    use rustix::io::Errno;

    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    match openat(parent, child, flags, Mode::empty()) {
        Ok(directory) => {
            // The child may have been created by a concurrent publisher that
            // has not yet synced this parent. Do not accept the visible entry
            // as crash-durable until this process supplies that barrier.
            fsync(parent).map_err(|error| {
                format!("could not durably accept cache component {child}: {error}")
            })?;
            return Ok(directory);
        }
        Err(Errno::NOENT) => {}
        Err(error) => {
            return Err(format!(
                "Virtual Spread cache component {child} is not a real directory: {error}"
            ))
        }
    }

    match mkdirat(
        parent,
        child,
        Mode::RWXU | Mode::RGRP | Mode::XGRP | Mode::ROTH | Mode::XOTH,
    ) {
        Ok(()) => {}
        Err(Errno::EXIST) => {}
        Err(error) => {
            return Err(format!(
                "could not create Virtual Spread cache component {child}: {error}"
            ))
        }
    }
    // Sync even after losing a concurrent mkdir race. The winning process may
    // not yet have persisted the parent entry when this publisher continues.
    fsync(parent)
        .map_err(|error| format!("could not durably create cache component {child}: {error}"))?;
    // Another idempotent publisher may have won the mkdir race. Re-open the
    // resulting entry without following symlinks and accept only a directory.
    openat(parent, child, flags, Mode::empty()).map_err(|error| {
        format!("Virtual Spread cache component {child} is not a real directory: {error}")
    })
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn verify_installed_bytes_at(
    directory: &std::os::fd::OwnedFd,
    name: &str,
    expected: &[u8],
    label: &str,
) -> Result<std::os::fd::OwnedFd, String> {
    use rustix::fs::{openat, Mode, OFlags};

    let file = openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| {
        format!("Virtual Spread {label} destination {name} is not a regular file: {error}")
    })?;
    verify_opened_bytes_at(directory, file, expected, label, name)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn verify_opened_bytes_at(
    directory: &std::os::fd::OwnedFd,
    file: std::os::fd::OwnedFd,
    expected: &[u8],
    label: &str,
    name: &str,
) -> Result<std::os::fd::OwnedFd, String> {
    verify_opened_bytes_at_with(directory, file, expected, label, name, || Ok(()))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn verify_opened_bytes_at_with<F>(
    directory: &std::os::fd::OwnedFd,
    file: std::os::fd::OwnedFd,
    expected: &[u8],
    label: &str,
    name: &str,
    after_byte_verification: F,
) -> Result<std::os::fd::OwnedFd, String>
where
    F: FnOnce() -> Result<(), String>,
{
    let mut file = File::from(file);
    let metadata = file
        .metadata()
        .map_err(|error| format!("could not inspect Virtual Spread {label} {name}: {error}"))?;
    if !metadata.is_file() || metadata.len() != expected.len() as u64 {
        return Err(format!(
            "Virtual Spread {label} destination {name} already contains different bytes"
        ));
    }
    verify_reader_bytes(&mut file, expected, label, name)?;
    after_byte_verification()?;
    let file = std::os::fd::OwnedFd::from(file);
    verify_leaf_is_still_attached(directory, name, &file, label)?;
    Ok(file)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn verify_leaf_is_still_attached(
    directory: &std::os::fd::OwnedFd,
    name: &str,
    opened: &std::os::fd::OwnedFd,
    label: &str,
) -> Result<(), String> {
    use rustix::fs::{fstat, statat, AtFlags};

    let expected = fstat(opened)
        .map_err(|error| format!("could not identify verified Virtual Spread {label}: {error}"))?;
    let actual = statat(directory, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|error| {
        format!("Virtual Spread {label} destination {name} changed during verification: {error}")
    })?;
    if expected.st_dev != actual.st_dev || expected.st_ino != actual.st_ino {
        return Err(format!(
            "Virtual Spread {label} destination {name} changed during verification"
        ));
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn sync_directory_at(directory: &std::os::fd::OwnedFd) -> Result<(), String> {
    rustix::fs::fsync(directory)
        .map_err(|error| format!("could not sync Virtual Spread cache directory: {error}"))
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn publish_cache_files(
    _shared_storage_root: &Path,
    _view: &VirtualSpreadViewEvidence,
    _generated_pdf: &[u8],
    _manifest: &[u8],
) -> Result<(), String> {
    Err("Virtual Spread device-cache materialization requires Linux/Android atomic directory publication".to_owned())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn verify_reader_bytes(
    file: &mut File,
    expected: &[u8],
    label: &str,
    display_name: &str,
) -> Result<(), String> {
    let mut offset = 0;
    let mut buffer = [0_u8; 64 * 1024];
    while offset < expected.len() {
        let chunk_len = buffer.len().min(expected.len() - offset);
        let read = file.read(&mut buffer[..chunk_len]).map_err(|error| {
            format!("could not read Virtual Spread {label} {display_name}: {error}")
        })?;
        if read == 0 || buffer[..read] != expected[offset..offset + read] {
            return Err(format!(
                "Virtual Spread {label} destination {display_name} already contains different bytes"
            ));
        }
        offset += read;
    }
    let mut extra = [0_u8; 1];
    let extra_read = file.read(&mut extra).map_err(|error| {
        format!("could not read Virtual Spread {label} {display_name}: {error}")
    })?;
    if extra_read != 0 {
        return Err(format!(
            "Virtual Spread {label} destination {display_name} already contains different bytes"
        ));
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGINAL: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn view(fill: char) -> VirtualSpreadViewEvidence {
        let document_id = format!("inkbridge-doc-v1-{ORIGINAL}");
        let hash = fill.to_string().repeat(64);
        let view_id = format!("inkbridge-view-v1-{hash}");
        VirtualSpreadViewEvidence {
            cache_basename: format!("{document_id}.{view_id}.virtual-spread.pdf"),
            document_id,
            view_id,
            generated_pdf_sha256: hash.clone(),
            manifest_sha256: hash.clone(),
            mapping_authority_sha256: hash,
        }
    }

    fn hydration(candidate: &VirtualSpreadViewEvidence) -> HydrationEvidence {
        HydrationEvidence {
            candidate_view_id: candidate.view_id.clone(),
            canonical_revision: 7,
            represented_source_pages: vec![142, 143],
            hydrated_mark_sha256: "d".repeat(64),
        }
    }

    #[test]
    fn clean_regeneration_requires_complete_hydration_verification_and_rollback() {
        let previous = view('a');
        let candidate = view('b');
        let mut transaction = CacheRegenerationTransaction::begin_clean(
            "transaction-1".to_owned(),
            ORIGINAL.to_owned(),
            7,
            vec![142, 143],
            Some(previous.clone()),
            candidate.clone(),
        )
        .unwrap();
        transaction.record_generated(&candidate).unwrap();
        transaction.record_hydration(hydration(&candidate)).unwrap();
        transaction
            .record_verification(
                CandidateVerificationEvidence {
                    candidate_view_id: candidate.view_id.clone(),
                    generated_pdf_sha256: candidate.generated_pdf_sha256.clone(),
                    manifest_sha256: candidate.manifest_sha256.clone(),
                    mapping_authority_sha256: candidate.mapping_authority_sha256.clone(),
                    hydrated_mark_sha256: "d".repeat(64),
                },
                RollbackEvidence {
                    previous_mapping_sha256: "e".repeat(64),
                    previous_view_id: Some(previous.view_id),
                },
            )
            .unwrap();
        transaction.commit_activation("f".repeat(64)).unwrap();
        assert_eq!(transaction.phase, CacheRegenerationPhase::Activated);
        transaction.validate_persisted().unwrap();
    }

    #[test]
    fn dirty_regeneration_cannot_generate_before_exporting_native_state() {
        let previous = view('a');
        let candidate = view('b');
        let mut transaction = CacheRegenerationTransaction::begin_dirty(
            "transaction-2".to_owned(),
            ORIGINAL.to_owned(),
            vec![142, 143],
            previous.clone(),
            candidate.clone(),
        )
        .unwrap();
        assert!(transaction.record_generated(&candidate).is_err());
        assert!(transaction
            .record_dirty_export(DirtyMarkExportEvidence {
                active_view_id: "wrong-view".to_owned(),
                exported_snapshot_sha256: "c".repeat(64),
                canonical_revision: 7,
            })
            .is_err());
        transaction
            .record_dirty_export(DirtyMarkExportEvidence {
                active_view_id: previous.view_id,
                exported_snapshot_sha256: "c".repeat(64),
                canonical_revision: 7,
            })
            .unwrap();
        transaction.validate_persisted().unwrap();

        let mut persisted = serde_json::to_value(&transaction).unwrap();
        persisted.as_object_mut().unwrap().remove("dirtyExport");
        let restored: CacheRegenerationTransaction = serde_json::from_value(persisted).unwrap();
        assert!(restored
            .validate_persisted()
            .unwrap_err()
            .contains("required export evidence"));

        transaction.record_generated(&candidate).unwrap();
    }

    #[test]
    fn hydration_must_cover_both_pages_at_one_revision() {
        let candidate = view('b');
        let mut transaction = CacheRegenerationTransaction::begin_clean(
            "transaction-3".to_owned(),
            ORIGINAL.to_owned(),
            7,
            vec![142, 143],
            None,
            candidate.clone(),
        )
        .unwrap();
        transaction.record_generated(&candidate).unwrap();
        let mut incomplete = hydration(&candidate);
        incomplete.represented_source_pages = vec![142];
        assert!(transaction.record_hydration(incomplete).is_err());
        let mut stale = hydration(&candidate);
        stale.canonical_revision = 6;
        assert!(transaction.record_hydration(stale).is_err());
        transaction.record_hydration(hydration(&candidate)).unwrap();
    }

    #[test]
    fn regeneration_never_reuses_the_active_cache_identity() {
        let active = view('a');
        let error = CacheRegenerationTransaction::begin_clean(
            "transaction-4".to_owned(),
            ORIGINAL.to_owned(),
            7,
            vec![142, 143],
            Some(active.clone()),
            active,
        )
        .unwrap_err();
        assert!(error.contains("new versioned candidate"));
    }

    #[test]
    fn persisted_transaction_rejects_reused_active_cache_identity() {
        let active = view('a');
        let mut transaction = CacheRegenerationTransaction::begin_clean(
            "transaction-5".to_owned(),
            ORIGINAL.to_owned(),
            7,
            vec![142, 143],
            Some(active.clone()),
            view('b'),
        )
        .unwrap();
        transaction.candidate_view = active;
        assert!(transaction
            .validate_persisted()
            .unwrap_err()
            .contains("new versioned candidate"));
    }

    #[test]
    fn view_identity_prefix_is_stripped_exactly_once() {
        let mut candidate = view('b');
        candidate.view_id = format!("inkbridge-view-v1-{}", candidate.view_id);
        candidate.cache_basename = format!(
            "{}.{}.virtual-spread.pdf",
            candidate.document_id, candidate.view_id
        );
        assert!(CacheRegenerationTransaction::begin_clean(
            "transaction-6".to_owned(),
            ORIGINAL.to_owned(),
            7,
            vec![142, 143],
            None,
            candidate,
        )
        .is_err());
    }

    #[test]
    fn persisted_transaction_rejects_injected_future_phase_evidence() {
        let candidate = view('b');
        let mut transaction = CacheRegenerationTransaction::begin_clean(
            "transaction-5".to_owned(),
            ORIGINAL.to_owned(),
            7,
            vec![142, 143],
            None,
            candidate.clone(),
        )
        .unwrap();
        transaction.hydration = Some(hydration(&candidate));
        assert!(transaction
            .validate_persisted()
            .unwrap_err()
            .contains("generation"));

        transaction.hydration = None;
        transaction.dirty_export = Some(DirtyMarkExportEvidence {
            active_view_id: "forged".to_owned(),
            exported_snapshot_sha256: "c".repeat(64),
            canonical_revision: 7,
        });
        assert!(transaction
            .validate_persisted()
            .unwrap_err()
            .contains("previous view"));

        transaction.dirty_export = None;
        transaction.transaction_id.clear();
        assert!(transaction
            .validate_persisted()
            .unwrap_err()
            .contains("transaction ID"));

        transaction.transaction_id = "transaction-5".to_owned();
        transaction.phase = CacheRegenerationPhase::RolledBack;
        transaction.active_mapping_sha256 = Some("f".repeat(64));
        assert!(transaction
            .validate_persisted()
            .unwrap_err()
            .contains("activation evidence"));
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn pinned_directory_verification_detects_a_replacement_symlink() {
        use rustix::fs::{open, Mode, OFlags};
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root_fd = open(
            root.path(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
        let candidate = view('b');
        let mut pinned = root_fd;
        for component in [
            ".inkbridge",
            "virtual-spread",
            "v1",
            candidate.document_id.as_str(),
            candidate.view_id.as_str(),
        ] {
            pinned = ensure_child_directory_at(&pinned, component).unwrap();
        }
        let original = root
            .path()
            .join(VIRTUAL_SPREAD_CACHE_RELATIVE_ROOT)
            .join(&candidate.document_id)
            .join(&candidate.view_id);
        let detached = root.path().join("detached-candidate");
        std::fs::rename(&original, &detached).unwrap();
        symlink(outside.path(), &original).unwrap();

        assert!(!outside.path().join("manifest.json").exists());
        assert!(verify_directory_is_still_attached(root.path(), &candidate, &pinned).is_err());
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn fifo_cache_leaf_is_rejected_without_blocking() {
        use rustix::fs::{mkfifoat, open, Mode, OFlags};

        let root = tempfile::tempdir().unwrap();
        let root_fd = open(
            root.path(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
        let pinned = ensure_child_directory_at(&root_fd, "candidate").unwrap();
        mkfifoat(&pinned, ".nomedia", Mode::RUSR | Mode::WUSR).unwrap();

        let error =
            verify_installed_bytes_at(&pinned, ".nomedia", b"", "media-index marker").unwrap_err();
        assert!(error.contains("not a regular file") || error.contains("different bytes"));
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn byte_verification_rejects_same_byte_leaf_replacement() {
        use rustix::fs::{open, openat, Mode, OFlags};

        let root = tempfile::tempdir().unwrap();
        let root_fd = open(
            root.path(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
        let pinned = ensure_child_directory_at(&root_fd, "candidate").unwrap();
        let leaf_path = root.path().join("candidate").join("manifest.json");
        let detached_path = root.path().join("candidate").join("detached.json");
        std::fs::write(&leaf_path, b"same bytes").unwrap();
        let opened = openat(
            &pinned,
            "manifest.json",
            OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();

        let error = verify_opened_bytes_at_with(
            &pinned,
            opened,
            b"same bytes",
            "test manifest",
            "manifest.json",
            || {
                std::fs::rename(&leaf_path, &detached_path).map_err(|error| error.to_string())?;
                std::fs::write(&leaf_path, b"same bytes").map_err(|error| error.to_string())
            },
        )
        .unwrap_err();

        assert!(error.contains("changed during verification"));
        assert_eq!(std::fs::read(&leaf_path).unwrap(), b"same bytes");
        assert_eq!(std::fs::read(&detached_path).unwrap(), b"same bytes");
    }
}
