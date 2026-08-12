use inkbridge_folder_transport::{
    FolderTransport, GcloudFolder, NativeBooxManifestBuilder, TransportAction, TransportConfig,
    TransportState,
};
use std::env;
use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime};

fn main() {
    if let Err(error) = run() {
        eprintln!("inkbridge-folder-transport: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let mut arguments = env::args().skip(1);
    let command = arguments.next().unwrap_or_default();
    if matches!(command.as_str(), "--help" | "-h" | "help" | "") {
        println!("{}", usage());
        return Ok(());
    }
    if !matches!(command.as_str(), "once" | "watch" | "status") {
        return Err(usage());
    }
    let mut config_path = None;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--config" => config_path = arguments.next().map(PathBuf::from),
            unknown => return Err(format!("unknown argument {unknown}\n\n{}", usage())),
        }
    }
    let config_path = config_path.ok_or_else(usage)?;
    let config = TransportConfig::load(&config_path)?;
    if command == "status" {
        let state = TransportState::load(&config.state_path)?;
        println!(
            "{}",
            serde_json::to_string_pretty(&state).map_err(|error| error.to_string())?
        );
        return Ok(());
    }
    let cloud = GcloudFolder::new(&config.gcloud_command, &config.bucket);
    let lock_path = config.state_path.with_extension("json.lock");
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    let _lock = ProcessLock::acquire(lock_path)?;
    let builder = NativeBooxManifestBuilder;
    let transport =
        FolderTransport::new(&cloud, &builder, Duration::from_secs(config.settle_seconds));
    let mut state = TransportState::load(&config.state_path)?;
    loop {
        for document in &config.documents {
            let report = transport.sync_document(document, &mut state, SystemTime::now())?;
            state.save(&config.state_path)?;
            for action in report.actions {
                print_action(action);
            }
        }
        if command == "once" {
            return Ok(());
        }
        thread::sleep(Duration::from_secs(config.poll_seconds));
    }
}

struct ProcessLock {
    path: PathBuf,
    file: Option<File>,
}

impl ProcessLock {
    fn acquire(path: PathBuf) -> Result<Self, String> {
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| {
                format!(
                    "another InkBridge folder transport may be using the state; could not create {}: {error}",
                    path.display()
                )
            })?;
        Ok(Self {
            path,
            file: Some(file),
        })
    }
}

impl Drop for ProcessLock {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = std::fs::remove_file(&self.path);
    }
}

fn print_action(action: TransportAction) {
    match action {
        TransportAction::Uploaded {
            side,
            local_path,
            object_path,
            source_revision,
            uploaded_bytes,
        } => println!(
            "Uploaded {side:?} revision {source_revision}: {} -> {object_path} ({uploaded_bytes} bytes)",
            local_path.display()
        ),
        TransportAction::Delivered {
            side,
            object_path,
            local_path,
            generation,
        } => println!(
            "Delivered {side:?} output {object_path}#{generation} -> {}",
            local_path.display()
        ),
        TransportAction::Deferred { side, reason } => {
            println!("Deferred {side:?}: {reason}")
        }
        TransportAction::Conflict { object_path } => {
            println!("CONFLICT: broker preserved {object_path}; automatic uploads are paused")
        }
    }
}

fn usage() -> String {
    "Usage:\n  inkbridge-folder-transport once --config <transport.json>\n  inkbridge-folder-transport watch --config <transport.json>\n  inkbridge-folder-transport status --config <transport.json>".to_owned()
}
