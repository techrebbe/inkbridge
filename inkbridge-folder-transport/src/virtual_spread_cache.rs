use serde::{Deserialize, Serialize};

pub const VIRTUAL_SPREAD_CACHE_TRANSACTION_SCHEMA_VERSION: u32 = 2;

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
}
