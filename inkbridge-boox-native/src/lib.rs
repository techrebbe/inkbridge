use inkbridge_convert::{
    build_document_baseline, build_manifest_from_document_baseline, parse_document_baseline_bytes,
};
use std::path::Path;

pub fn build_baseline_json(pdf_path: &Path, source_file_name: &str) -> Result<Vec<u8>, String> {
    let baseline = build_document_baseline(pdf_path, source_file_name)?;
    to_pretty_json(&baseline)
}

pub fn build_manifest_json(
    pdf_path: &Path,
    baseline_bytes: &[u8],
    normalized_y_offset: f64,
) -> Result<Vec<u8>, String> {
    let baseline = parse_document_baseline_bytes(baseline_bytes, "BOOX device baseline")?;
    let manifest = build_manifest_from_document_baseline(pdf_path, &baseline, normalized_y_offset)?;
    to_pretty_json(&manifest)
}

fn to_pretty_json(value: &impl serde::Serialize) -> Result<Vec<u8>, String> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("could not serialize InkBridge JSON: {error}"))?;
    bytes.push(b'\n');
    Ok(bytes)
}

#[cfg(feature = "jni-bridge")]
mod jni_bridge {
    use super::{build_baseline_json, build_manifest_json};
    use jni::objects::{JByteArray, JObject, JString};
    use jni::strings::JNIString;
    use jni::{Env, EnvUnowned};
    use std::path::Path;

    fn throw(env: &mut Env<'_>, message: impl Into<String>) -> jni::errors::Error {
        if let Ok(class) = env.find_class(JNIString::new("java/lang/RuntimeException")) {
            let _ = env.throw_new(class, JNIString::new(message.into()));
        }
        jni::errors::Error::JavaException
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_inkbridge_boox_NativeBooxManifestConverter_nativeBuildBaseline<
        'local,
    >(
        mut env: EnvUnowned<'local>,
        _instance: JObject<'local>,
        path: JString<'local>,
        source_file_name: JString<'local>,
    ) -> JByteArray<'local> {
        env.with_env(|env| -> jni::errors::Result<JByteArray<'local>> {
            let path: String = path.try_to_string(env)?;
            let source_file_name: String = source_file_name.try_to_string(env)?;
            let bytes = build_baseline_json(Path::new(&path), &source_file_name)
                .map_err(|error| throw(env, error))?;
            env.byte_array_from_slice(&bytes)
        })
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_inkbridge_boox_NativeBooxManifestConverter_nativeBuildManifest<
        'local,
    >(
        mut env: EnvUnowned<'local>,
        _instance: JObject<'local>,
        path: JString<'local>,
        baseline_bytes: JByteArray<'local>,
        normalized_y_offset: f64,
    ) -> JByteArray<'local> {
        env.with_env(|env| -> jni::errors::Result<JByteArray<'local>> {
            let path: String = path.try_to_string(env)?;
            let baseline = env.convert_byte_array(&baseline_bytes)?;
            let bytes = build_manifest_json(Path::new(&path), &baseline, normalized_y_offset)
                .map_err(|error| throw(env, error))?;
            env.byte_array_from_slice(&bytes)
        })
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires private before/after NeoReader PDFs"]
    fn real_device_pair_matches_the_proven_desktop_operation_set() {
        let before = std::env::var("INKBRIDGE_BOOX_BASE_PDF")
            .expect("set INKBRIDGE_BOOX_BASE_PDF to the broker-generated view");
        let after = std::env::var("INKBRIDGE_BOOX_EDITED_PDF")
            .expect("set INKBRIDGE_BOOX_EDITED_PDF to the closed NeoReader view");
        let baseline = build_document_baseline(Path::new(&before), "large-test.pdf").unwrap();
        assert_eq!(baseline.source_file_name, "large-test.pdf");
        assert!(
            baseline.strokes.is_empty(),
            "this parity fixture must begin without editable ink"
        );
        let device =
            build_manifest_from_document_baseline(Path::new(&after), &baseline, -0.0008).unwrap();
        assert_eq!(device.document.source_file_name, "large-test.pdf");
        let desktop = inkbridge_convert::build_manifest(Path::new(&after), &[], -0.0008).unwrap();

        assert_eq!(device.operations, desktop.operations);
        assert_eq!(device.summary, desktop.summary);
        assert_eq!(device.coordinate_transform, desktop.coordinate_transform);
        assert_eq!(device.document.pdf_sha256, desktop.document.pdf_sha256);
    }
    #[test]
    fn malformed_device_baseline_is_rejected_before_pdf_access() {
        let error = build_manifest_json(
            Path::new("missing.pdf"),
            br#"{"schemaVersion":99}"#,
            -0.0008,
        )
        .expect_err("invalid baseline must stop before opening the PDF");
        assert!(error.contains("invalid BOOX baseline JSON") || error.contains("unsupported"));
        assert!(!error.contains("missing.pdf"));
    }
}
