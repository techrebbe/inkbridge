use super::*;
use lopdf::Document;

pub const VIRTUAL_SPREAD_PAGE_143_SOURCE_PDF_SHA256: &str =
    "c9271098e6d98f7fff378c4d630dc9c179cf45cb5283f3559eee910e3afafeb4";
pub const VIRTUAL_SPREAD_PAGE_143_GENERATED_PDF_SHA256: &str =
    "0c895249809a36f382312ae42547ec2f9755e0b4095ce2b8e8a5f6145be3a32f";
pub const VIRTUAL_SPREAD_PAGE_143_SIDECAR_SHA256: &str =
    "37cda3d96db8b2f8f311df60ccfbbd397bbb446b9e4a7451dcbbffc283aff9df";
pub const VIRTUAL_SPREAD_PAGE_143_ARTIFACT_DESCRIPTOR_SHA256: &str =
    "87916b5bea80f7831a08f8531a4dd3b57946ee53d041c00b686e2e8de2df082e";
pub const VIRTUAL_SPREAD_PAGE_143_PDF_TAIL_SHA256: &str =
    "c58b5a5279bca1024e6e25f6902e589fa98c08007723e8f5f7420fa6f583e89d";

const PAGE_143_MAPPING_AUTHORITY_SHA256: &str =
    "646b905c12266774882e0c4d7ebbbca77b2f386f432979ebcbfcda1d9ace268a";
const PAGE_143_LAYOUT_AUTHORITY_SHA256: &str =
    "5511205907d3de274d63449c9cb128300b7acbb8fa191415d387f861626fe94e";
const PAGE_143_LINK_AUTHORITY_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const PAGE_143_VIEW_ID: &str =
    "inkbridge-view-v1-7cb2c2fda17d5510d33b0a97e702cbc66d5124735be45f810aef6053c1775f30";
const PAGE_143_CACHE_BASENAME: &str = "inkbridge-doc-v1-c9271098e6d98f7fff378c4d630dc9c179cf45cb5283f3559eee910e3afafeb4.inkbridge-view-v1-7cb2c2fda17d5510d33b0a97e702cbc66d5124735be45f810aef6053c1775f30.virtual-spread.pdf";
const PAGE_143_AUTHORITY_BLOCK_OFFSET: usize = 5_844;
const PAGE_143_STARTXREF_OFFSET: usize = 6_312;
const PAGE_143_SOURCE_SIZE: usize = 3_768;
const PAGE_143_GENERATED_SIZE: usize = 6_333;
const PAGE_143_SIDECAR_SIZE: usize = 6_371;
const PAGE_143_ARTIFACT_DESCRIPTOR_SIZE: usize = 6_364;

/// Byte-level verification result for RTL Reader's normative real-PDF fixture.
///
/// This proves that InkBridge interprets the same immutable source, generated
/// PDF, sidecar, transform authority, view identity, and PDF-tail authorities
/// as RTL Reader v0.0.25. It does not prove native `.mark` hydration and does
/// not by itself enable device cache activation.
#[derive(Clone, Debug, PartialEq)]
pub struct VirtualSpreadProductionVerification {
    manifest: VirtualSpreadManifest,
    source_pdf_sha256: String,
    generated_pdf_sha256: String,
    sidecar_sha256: String,
    artifact_descriptor_sha256: String,
    pdf_tail_sha256: String,
    authority_block_offset: usize,
    startxref_offset: usize,
}

impl VirtualSpreadProductionVerification {
    pub fn manifest(&self) -> &VirtualSpreadManifest {
        &self.manifest
    }

    pub fn source_pdf_sha256(&self) -> &str {
        &self.source_pdf_sha256
    }

    pub fn generated_pdf_sha256(&self) -> &str {
        &self.generated_pdf_sha256
    }

    pub fn sidecar_sha256(&self) -> &str {
        &self.sidecar_sha256
    }

    pub fn artifact_descriptor_sha256(&self) -> &str {
        &self.artifact_descriptor_sha256
    }

    pub fn pdf_tail_sha256(&self) -> &str {
        &self.pdf_tail_sha256
    }

    pub const fn authority_block_offset(&self) -> usize {
        self.authority_block_offset
    }

    pub const fn startxref_offset(&self) -> usize {
        self.startxref_offset
    }
}

/// Verify the exact real-PDF handoff merged in RTL Reader PR #18.
///
/// All five artifacts are content-addressed. The sidecar is then independently
/// parsed through InkBridge's strict schema-v3 verifier, the two PDFs are opened
/// semantically, and the authenticated authority tail is checked at its frozen
/// byte offsets. Any mismatch fails closed.
pub fn verify_virtual_spread_page_143_production_fixture(
    source_pdf: &[u8],
    generated_pdf: &[u8],
    sidecar: &[u8],
    artifact_descriptor: &[u8],
    pdf_tail: &[u8],
) -> Result<VirtualSpreadProductionVerification, String> {
    verify_exact_artifact(
        source_pdf,
        PAGE_143_SOURCE_SIZE,
        VIRTUAL_SPREAD_PAGE_143_SOURCE_PDF_SHA256,
        "source PDF",
    )?;
    verify_exact_artifact(
        generated_pdf,
        PAGE_143_GENERATED_SIZE,
        VIRTUAL_SPREAD_PAGE_143_GENERATED_PDF_SHA256,
        "generated PDF",
    )?;
    verify_exact_artifact(
        sidecar,
        PAGE_143_SIDECAR_SIZE,
        VIRTUAL_SPREAD_PAGE_143_SIDECAR_SHA256,
        "schema-v3 sidecar",
    )?;
    verify_exact_artifact(
        artifact_descriptor,
        PAGE_143_ARTIFACT_DESCRIPTOR_SIZE,
        VIRTUAL_SPREAD_PAGE_143_ARTIFACT_DESCRIPTOR_SHA256,
        "artifact descriptor",
    )?;
    verify_exact_artifact(
        pdf_tail,
        PAGE_143_GENERATED_SIZE - PAGE_143_AUTHORITY_BLOCK_OFFSET,
        VIRTUAL_SPREAD_PAGE_143_PDF_TAIL_SHA256,
        "PDF-tail evidence",
    )?;

    let descriptor = strict_json_value(artifact_descriptor)?;
    if !descriptor.is_object() {
        return Err("Virtual Spread artifact descriptor is not an object".to_owned());
    }

    let manifest =
        parse_virtual_spread_manifest(sidecar, VIRTUAL_SPREAD_PAGE_143_SOURCE_PDF_SHA256)?;
    validate_fixture_manifest(&manifest)?;
    validate_pdf_page_count(source_pdf, 3, "source")?;
    validate_pdf_page_count(generated_pdf, 2, "generated")?;
    validate_authority_tail(generated_pdf, pdf_tail, &manifest)?;

    Ok(VirtualSpreadProductionVerification {
        manifest,
        source_pdf_sha256: VIRTUAL_SPREAD_PAGE_143_SOURCE_PDF_SHA256.to_owned(),
        generated_pdf_sha256: VIRTUAL_SPREAD_PAGE_143_GENERATED_PDF_SHA256.to_owned(),
        sidecar_sha256: VIRTUAL_SPREAD_PAGE_143_SIDECAR_SHA256.to_owned(),
        artifact_descriptor_sha256: VIRTUAL_SPREAD_PAGE_143_ARTIFACT_DESCRIPTOR_SHA256.to_owned(),
        pdf_tail_sha256: VIRTUAL_SPREAD_PAGE_143_PDF_TAIL_SHA256.to_owned(),
        authority_block_offset: PAGE_143_AUTHORITY_BLOCK_OFFSET,
        startxref_offset: PAGE_143_STARTXREF_OFFSET,
    })
}

fn verify_exact_artifact(
    bytes: &[u8],
    expected_size: usize,
    expected_sha256: &str,
    label: &str,
) -> Result<(), String> {
    if bytes.len() != expected_size {
        return Err(format!(
            "Virtual Spread {label} size mismatch: expected {expected_size}, got {}",
            bytes.len()
        ));
    }
    if hex_sha256(bytes) != expected_sha256 {
        return Err(format!("Virtual Spread {label} SHA-256 mismatch"));
    }
    Ok(())
}

fn validate_fixture_manifest(manifest: &VirtualSpreadManifest) -> Result<(), String> {
    if manifest.generated_pdf_sha256 != VIRTUAL_SPREAD_PAGE_143_GENERATED_PDF_SHA256
        || manifest.layout_authority_sha256 != PAGE_143_LAYOUT_AUTHORITY_SHA256
        || manifest.link_authority_sha256 != PAGE_143_LINK_AUTHORITY_SHA256
        || manifest.mapping_authority_sha256 != PAGE_143_MAPPING_AUTHORITY_SHA256
        || manifest.view_id != PAGE_143_VIEW_ID
        || manifest.cache_basename != PAGE_143_CACHE_BASENAME
        || manifest.original_page_count != 3
        || manifest.generated_page_count != 2
    {
        return Err(
            "Virtual Spread production manifest disagrees with frozen fixture authority".to_owned(),
        );
    }
    let mapping = manifest
        .mapping_for_source_page(2)
        .ok_or_else(|| "Virtual Spread production fixture has no page-143 mapping".to_owned())?;
    if mapping.virtual_page_index != 1
        || mapping.side != VirtualSpreadSide::Left
        || mapping.source_rotation != 90
        || !bits_equal(mapping.source_box, [18.0, 36.0, 594.0, 756.0])
        || !bits_equal(mapping.normalized_source_box, [36.0, 18.0, 756.0, 594.0])
        || !bits_equal(mapping.slot, [0.0, 0.0, 432.0, 648.0])
        || !bits_equal(
            mapping.destination,
            [0.0, 151.20000000000002, 432.0, 496.79999999999995],
        )
        || mapping.scale.to_bits() != 0.6f64.to_bits()
        || !bits_equal(
            mapping.forward.coefficients(),
            [0.0, -0.6, 0.6, 0.0, -21.599999999999998, 507.6],
        )
    {
        return Err("Virtual Spread production page-143 mapping drifted".to_owned());
    }
    Ok(())
}

fn bits_equal<const N: usize>(actual: [f64; N], expected: [f64; N]) -> bool {
    actual
        .iter()
        .zip(expected)
        .all(|(actual, expected)| actual.to_bits() == expected.to_bits())
}

fn validate_pdf_page_count(bytes: &[u8], expected: usize, label: &str) -> Result<(), String> {
    let document = Document::load_mem(bytes)
        .map_err(|error| format!("could not open Virtual Spread {label} PDF: {error}"))?;
    let actual = document.get_pages().len();
    if actual != expected {
        return Err(format!(
            "Virtual Spread {label} PDF page count mismatch: expected {expected}, got {actual}"
        ));
    }
    Ok(())
}

fn validate_authority_tail(
    generated_pdf: &[u8],
    pdf_tail: &[u8],
    manifest: &VirtualSpreadManifest,
) -> Result<(), String> {
    if generated_pdf.get(PAGE_143_AUTHORITY_BLOCK_OFFSET..) != Some(pdf_tail) {
        return Err("Virtual Spread PDF-tail evidence is not the generated PDF suffix".to_owned());
    }
    if generated_pdf
        .get(PAGE_143_STARTXREF_OFFSET..)
        .is_none_or(|bytes| !bytes.starts_with(b"startxref\n"))
    {
        return Err("Virtual Spread startxref is not at the frozen offset".to_owned());
    }
    let view_hash = manifest
        .view_id
        .strip_prefix(VIEW_ID_PREFIX)
        .ok_or_else(|| "Virtual Spread production view ID has an unsupported prefix".to_owned())?;
    let authority = format!(
        "%SNVirtualSpreadSourceSHA256:{}\n\
%SNVirtualSpreadLayoutSHA256:{}\n\
%SNVirtualSpreadLinksSHA256:{}\n\
%SNVirtualSpreadMappingSHA256:{}\n\
%SNVirtualSpreadViewSHA256:{}\n",
        manifest.original_pdf_sha256,
        manifest.layout_authority_sha256,
        manifest.link_authority_sha256,
        manifest.mapping_authority_sha256,
        view_hash
    );
    if generated_pdf.get(PAGE_143_AUTHORITY_BLOCK_OFFSET..PAGE_143_STARTXREF_OFFSET)
        != Some(authority.as_bytes())
    {
        return Err("Virtual Spread PDF authority block does not match the sidecar".to_owned());
    }
    Ok(())
}
