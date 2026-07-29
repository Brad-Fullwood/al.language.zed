//! Regenerate the checked-in Record method catalog from Microsoft's AL toolchain.
//!
//! Usage:
//! `cargo run -p al-semantic --features semantic --example export_record_methods -- \
//!   <Microsoft.Dynamics.Nav.CodeAnalysis.dll> <record_methods.json>`

use std::collections::BTreeSet;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::UNIX_EPOCH;

use al_semantic::SemanticBridge;
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RecordMethodCatalog {
    source_type: &'static str,
    toolchain_version: String,
    methods: Vec<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("record-method export failed: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn Error>> {
    let (code_analysis, output) = parse_args(std::env::args_os().skip(1))?;
    if !code_analysis.is_file() {
        return Err(format!(
            "CodeAnalysis assembly does not exist: {}",
            code_analysis.display()
        )
        .into());
    }

    let metadata = code_analysis.metadata()?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs());
    let toolchain_version =
        discover_extension_version(&code_analysis).unwrap_or_else(|| "unknown".to_string());
    let cache_version = format!(
        "record-method-catalog-{toolchain_version}-{}-{modified}",
        metadata.len()
    );

    let bridge = SemanticBridge::new(&code_analysis, &cache_version)?;
    let builtins = bridge.builtin_types_fresh().await?;
    let table_class = builtins
        .iter()
        .find(|builtin| builtin.name.eq_ignore_ascii_case("TableClass"))
        .ok_or("Microsoft built-in catalog has no TableClass type")?;
    let methods: Vec<String> = table_class
        .methods
        .iter()
        .map(|method| method.name.trim())
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if methods.len() < 50 {
        return Err(format!(
            "TableClass catalog is implausibly small: {} unique methods",
            methods.len()
        )
        .into());
    }

    let catalog = RecordMethodCatalog {
        source_type: "TableClass",
        toolchain_version,
        methods,
    };
    let mut bytes = serde_json::to_vec_pretty(&catalog)?;
    bytes.push(b'\n');
    persist_atomically(&output, &bytes)?;

    eprintln!(
        "wrote {} Record methods to {}",
        catalog.methods.len(),
        output.display()
    );
    Ok(())
}

fn parse_args(args: impl Iterator<Item = OsString>) -> Result<(PathBuf, PathBuf), Box<dyn Error>> {
    let args: Vec<OsString> = args.collect();
    if args.len() != 2 {
        return Err(
            "usage: export_record_methods <Microsoft.Dynamics.Nav.CodeAnalysis.dll> <output.json>"
                .into(),
        );
    }
    Ok((PathBuf::from(&args[0]), PathBuf::from(&args[1])))
}

fn discover_extension_version(code_analysis: &Path) -> Option<String> {
    for ancestor in code_analysis.ancestors() {
        let package_json = ancestor.join("package.json");
        let Ok(bytes) = fs::read(package_json) else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        if value.get("name").and_then(serde_json::Value::as_str) == Some("al")
            || value
                .get("publisher")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|publisher| publisher.eq_ignore_ascii_case("microsoft"))
        {
            return value
                .get("version")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned);
        }
    }
    None
}

fn persist_atomically(output: &Path, bytes: &[u8]) -> Result<(), Box<dyn Error>> {
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(output)?;
    Ok(())
}
