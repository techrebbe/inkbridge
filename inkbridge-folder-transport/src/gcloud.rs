use crate::CloudObject;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub trait CloudFolder {
    fn list(&self, prefix: &str) -> Result<Vec<CloudObject>, String>;
    fn upload_create(
        &self,
        local_path: &Path,
        object_path: &str,
        metadata: &BTreeMap<String, String>,
    ) -> Result<CloudObject, String>;
    fn download(&self, object: &CloudObject, destination: &Path) -> Result<(), String>;
}

#[derive(Clone, Debug)]
pub struct GcloudFolder {
    command: PathBuf,
    bucket: String,
}

impl GcloudFolder {
    pub fn new(command: impl Into<PathBuf>, bucket: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            bucket: bucket.into(),
        }
    }

    fn gs_url(&self, path: &str) -> String {
        format!("gs://{}/{}", self.bucket, path)
    }

    fn run(&self, arguments: &[String]) -> Result<Output, String> {
        Command::new(&self.command)
            .args(arguments)
            .output()
            .map_err(|error| format!("could not run {}: {error}", self.command.display()))
    }

    fn describe(&self, path: &str) -> Result<CloudObject, String> {
        let output = self.run(&[
            "storage".to_owned(),
            "objects".to_owned(),
            "describe".to_owned(),
            self.gs_url(path),
            "--format=json".to_owned(),
            "--quiet".to_owned(),
        ])?;
        if !output.status.success() {
            return Err(command_error("describe Cloud Storage object", &output));
        }
        let raw: DescribeObject = serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("invalid gcloud describe JSON: {error}"))?;
        raw.into_cloud_object()
    }
}

impl CloudFolder for GcloudFolder {
    fn list(&self, prefix: &str) -> Result<Vec<CloudObject>, String> {
        let pattern = format!("{}/**", self.gs_url(prefix.trim_end_matches('/')));
        let output = self.run(&[
            "storage".to_owned(),
            "ls".to_owned(),
            "--json".to_owned(),
            pattern,
            "--quiet".to_owned(),
        ])?;
        if !output.status.success() {
            let message = String::from_utf8_lossy(&output.stderr);
            if message.contains("matched no objects") || message.contains("No URLs matched") {
                return Ok(Vec::new());
            }
            return Err(command_error("list Cloud Storage objects", &output));
        }
        let rows: Vec<ListObject> = serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("invalid gcloud list JSON: {error}"))?;
        rows.into_iter()
            .filter(|row| row.kind == "cloud_object")
            .map(|row| row.metadata.into_cloud_object())
            .collect()
    }

    fn upload_create(
        &self,
        local_path: &Path,
        object_path: &str,
        metadata: &BTreeMap<String, String>,
    ) -> Result<CloudObject, String> {
        if !is_device_upload_path(object_path) {
            return Err(format!(
                "folder transport may upload only within BOOX_Folder/ or Supernote_Folder/: {object_path}"
            ));
        }
        let metadata = metadata
            .iter()
            .map(|(key, value)| {
                if key.contains([',', '=']) || value.contains([',', '=']) {
                    Err(format!("metadata {key} contains a gcloud-unsafe delimiter"))
                } else {
                    Ok(format!("{key}={value}"))
                }
            })
            .collect::<Result<Vec<_>, _>>()?
            .join(",");
        let output = self.run(&[
            "storage".to_owned(),
            "cp".to_owned(),
            local_path.to_string_lossy().into_owned(),
            self.gs_url(object_path),
            format!("--custom-metadata={metadata}"),
            "--if-generation-match=0".to_owned(),
            "--quiet".to_owned(),
        ])?;
        if output.status.success() {
            return self.describe(object_path);
        }

        // The object name is content/revision-stable. A retry after an uncertain
        // response is successful only if the existing immutable object carries
        // exactly the metadata we intended to publish.
        if let Ok(existing) = self.describe(object_path) {
            if metadata_matches(&existing.metadata, metadata.as_str()) {
                return Ok(existing);
            }
        }
        Err(command_error("upload Cloud Storage object", &output))
    }

    fn download(&self, object: &CloudObject, destination: &Path) -> Result<(), String> {
        let source = format!("{}#{}", self.gs_url(&object.path), object.generation);
        let output = self.run(&[
            "storage".to_owned(),
            "cp".to_owned(),
            source,
            destination.to_string_lossy().into_owned(),
            "--quiet".to_owned(),
        ])?;
        if output.status.success() {
            Ok(())
        } else {
            Err(command_error("download Cloud Storage object", &output))
        }
    }
}

fn is_device_upload_path(path: &str) -> bool {
    (path.starts_with("BOOX_Folder/") || path.starts_with("Supernote_Folder/"))
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
        && !path.chars().any(char::is_control)
}

fn metadata_matches(existing: &BTreeMap<String, String>, encoded: &str) -> bool {
    encoded.split(',').all(|pair| {
        pair.split_once('=')
            .is_some_and(|(key, value)| existing.get(key).is_some_and(|found| found == value))
    })
}

fn command_error(action: &str, output: &Output) -> String {
    let detail = String::from_utf8_lossy(if output.stderr.is_empty() {
        &output.stdout
    } else {
        &output.stderr
    });
    format!("could not {action}: {}", detail.trim())
}

#[derive(Deserialize)]
struct ListObject {
    #[serde(rename = "type")]
    kind: String,
    metadata: ApiObject,
}

#[derive(Deserialize)]
struct ApiObject {
    name: String,
    generation: String,
    size: String,
    #[serde(default)]
    metadata: BTreeMap<String, String>,
}

impl ApiObject {
    fn into_cloud_object(self) -> Result<CloudObject, String> {
        Ok(CloudObject {
            path: self.name,
            generation: self
                .generation
                .parse()
                .map_err(|error| format!("invalid Cloud Storage generation: {error}"))?,
            size: self
                .size
                .parse()
                .map_err(|error| format!("invalid Cloud Storage object size: {error}"))?,
            metadata: self.metadata,
        })
    }
}

#[derive(Deserialize)]
struct DescribeObject {
    name: String,
    generation: String,
    size: u64,
    #[serde(default)]
    custom_fields: BTreeMap<String, String>,
}

impl DescribeObject {
    fn into_cloud_object(self) -> Result<CloudObject, String> {
        Ok(CloudObject {
            path: self.name,
            generation: self
                .generation
                .parse()
                .map_err(|error| format!("invalid Cloud Storage generation: {error}"))?,
            size: self.size,
            metadata: self.custom_fields,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_paths_are_confined_to_device_namespaces() {
        assert!(is_device_upload_path(
            "BOOX_Folder/inkbridge-doc-v1-test/uploads/r1.json"
        ));
        assert!(is_device_upload_path(
            "Supernote_Folder/inkbridge-doc-v1-test/uploads/r1.json"
        ));
        for path in [
            "Conflicts/inkbridge-doc-v1-test/event/resolution.json",
            "Canonical/inkbridge-doc-v1-test/state.json",
            "BOOX_Folder/",
            "BOOX_Folder/../Conflicts/event/resolution.json",
            "Supernote_Folder//event.json",
        ] {
            assert!(!is_device_upload_path(path), "{path}");
        }
    }

    #[test]
    fn parses_gcloud_list_shape_and_preserves_custom_metadata() {
        let rows: Vec<ListObject> = serde_json::from_str(
            r#"[{
              "url": "gs://bucket/Folder/doc/object#17",
              "type": "cloud_object",
              "metadata": {
                "name": "Folder/doc/object",
                "generation": "17",
                "size": "42",
                "metadata": {
                  "inkbridge-document-id": "inkbridge-doc-v1-test",
                  "inkbridge-source-revisions": "2:3"
                }
              }
            }]"#,
        )
        .unwrap();
        let object = rows
            .into_iter()
            .next()
            .unwrap()
            .metadata
            .into_cloud_object()
            .unwrap();
        assert_eq!(object.path, "Folder/doc/object");
        assert_eq!(object.generation, 17);
        assert_eq!(object.size, 42);
        assert_eq!(object.metadata["inkbridge-source-revisions"], "2:3");
    }

    #[test]
    fn parses_gcloud_describe_shape() {
        let raw: DescribeObject = serde_json::from_str(
            r#"{
              "name": "Folder/doc/object",
              "generation": "19",
              "size": 81,
              "custom_fields": {"inkbridge-content-sha256": "abc"}
            }"#,
        )
        .unwrap();
        let object = raw.into_cloud_object().unwrap();
        assert_eq!(object.generation, 19);
        assert_eq!(object.metadata["inkbridge-content-sha256"], "abc");
    }

    #[test]
    fn uncertain_upload_retry_requires_all_intended_metadata() {
        let existing = BTreeMap::from([
            ("a".to_owned(), "1".to_owned()),
            ("b".to_owned(), "2".to_owned()),
        ]);
        assert!(metadata_matches(&existing, "a=1,b=2"));
        assert!(!metadata_matches(&existing, "a=1,b=wrong"));
        assert!(!metadata_matches(&existing, "a=1,c=3"));
    }
}
