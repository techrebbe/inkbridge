use inkbridge_convert::build_manifest;
use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    if let Err(error) = run() {
        eprintln!("inkbridge-convert: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let command = args.next().unwrap_or_default();
    if command != "extract" {
        return Err(usage());
    }

    let mut pdf: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut baselines = Vec::new();
    let mut y_offset = -0.0008f64;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--pdf" => pdf = Some(PathBuf::from(required_value(&mut args, "--pdf")?)),
            "--baseline" => baselines.push(PathBuf::from(required_value(&mut args, "--baseline")?)),
            "--output" => output = Some(PathBuf::from(required_value(&mut args, "--output")?)),
            "--y-offset" => {
                let value = required_value(&mut args, "--y-offset")?;
                y_offset = value
                    .parse()
                    .map_err(|_| format!("invalid --y-offset value: {value}"))?;
            }
            "--help" | "-h" => return Err(usage()),
            unknown => return Err(format!("unknown argument {unknown}\n\n{}", usage())),
        }
    }

    let pdf = pdf.ok_or_else(usage)?;
    let output = output.ok_or_else(usage)?;
    let manifest = build_manifest(&pdf, &baselines, y_offset)?;
    let json = serde_json::to_string_pretty(&manifest)
        .map_err(|error| format!("could not serialize manifest: {error}"))?;
    fs::write(&output, format!("{json}\n"))
        .map_err(|error| format!("could not write {}: {error}", output.display()))?;
    println!(
        "Created {}: {} upserted, {} deleted, {} unchanged, {} skipped",
        output.display(),
        manifest.summary.upserted,
        manifest.summary.deleted,
        manifest.summary.unchanged,
        manifest.summary.skipped
    );
    Ok(())
}

fn required_value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn usage() -> String {
    "Usage:\n  inkbridge-convert extract --pdf <NeoReader-embedded.pdf> \\\n    --baseline <Supernote-page-export.log|json> [--baseline ...] \\\n    --output <inkbridge-manifest.json> [--y-offset -0.0008]".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_continuation_lines_are_copyable_shell_arguments() {
        let text = usage();
        assert!(text
            .lines()
            .skip(1)
            .all(|line| !line.trim().starts_with('+')));
        assert!(text.contains("\n    --baseline"));
        assert!(text.contains("\n    --output"));
    }
}
