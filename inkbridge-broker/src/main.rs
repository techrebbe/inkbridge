use inkbridge_broker::{stable_document_id, StorageEvent, EVENT_SCHEMA_VERSION};
use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    if let Err(error) = run() {
        eprintln!("inkbridge-broker: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let mut arguments = env::args().skip(1);
    match arguments.next().as_deref() {
        Some("document-id") => {
            let pdf = PathBuf::from(arguments.next().ok_or_else(usage)?);
            if arguments.next().is_some() {
                return Err(usage());
            }
            let bytes = fs::read(&pdf)
                .map_err(|error| format!("could not read {}: {error}", pdf.display()))?;
            lopdf::Document::load_mem(&bytes)
                .map_err(|error| format!("{} is not a readable PDF: {error}", pdf.display()))?;
            println!("{}", stable_document_id(&bytes));
            Ok(())
        }
        Some("validate-event") => {
            let event_path = PathBuf::from(arguments.next().ok_or_else(usage)?);
            if arguments.next().is_some() {
                return Err(usage());
            }
            let bytes = fs::read(&event_path)
                .map_err(|error| format!("could not read {}: {error}", event_path.display()))?;
            let event: StorageEvent = serde_json::from_slice(&bytes)
                .map_err(|error| format!("invalid event JSON: {error}"))?;
            if event.schema_version != EVENT_SCHEMA_VERSION {
                return Err(format!(
                    "unsupported event schema version {}",
                    event.schema_version
                ));
            }
            println!(
                "valid event {} for {} ({:?} revision {})",
                event.event_id, event.document_id, event.source, event.source_revision
            );
            Ok(())
        }
        Some("--help") | Some("-h") | None => Err(usage()),
        Some(command) => Err(format!("unknown command {command}\n\n{}", usage())),
    }
}

fn usage() -> String {
    "Usage:\n  inkbridge-broker document-id <immutable-original.pdf>\n  inkbridge-broker validate-event <storage-event.json>\n\nThe storage-independent Broker library is exercised locally by `cargo test -p inkbridge-broker`; Cloud Storage and Firestore adapters are intentionally deferred.".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_names_the_local_core_entry_points() {
        let text = usage();
        assert!(text.contains("document-id"));
        assert!(text.contains("validate-event"));
        assert!(text.contains("storage-independent"));
    }
}
