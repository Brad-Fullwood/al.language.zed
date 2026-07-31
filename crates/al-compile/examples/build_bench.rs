//! Benchmark driver for the release-report harness.
//!
//! One process runs both compiler backends against the same project in three
//! states: process/package-index cold, warm unchanged, and a deterministic
//! one-file edit.
//! The Python harness starts a fresh driver for every round and alternates the
//! backend order, so neither backend owns the machine-load or filesystem-cache
//! advantage. This is an example target rather than a shipped product binary.

use std::path::{Path, PathBuf};
use std::time::Instant;

use al_compile::{BuildBackend, BuildRequest, CompilationConfigOptions, DiagnosticSeverity};
use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Measurement {
    backend: &'static str,
    scenario: &'static str,
    elapsed_ns: u64,
    load_average: Option<f64>,
    success: bool,
    app_size: u64,
    diagnostics: usize,
    errors: usize,
    timings: Option<al_emit::BuildTimings>,
    artifact: Option<PathBuf>,
    failure: Option<String>,
}

struct EditGuard {
    path: PathBuf,
    original: Vec<u8>,
}

impl EditGuard {
    fn apply(path: PathBuf) -> std::io::Result<Self> {
        let original = std::fs::read(&path)?;
        let mut edited = original.clone();
        if !edited.ends_with(b"\n") {
            edited.push(b'\n');
        }
        edited.extend_from_slice(b"// benchmark one-file edit\n");
        std::fs::write(&path, edited)?;
        Ok(Self { path, original })
    }
}

impl Drop for EditGuard {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.path, &self.original);
    }
}

fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn load_average() -> Option<f64> {
    std::fs::read_to_string("/proc/loadavg")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn first_al_file(root: &Path) -> std::io::Result<PathBuf> {
    fn visit(dir: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name == ".alpackages") {
                    continue;
                }
                visit(&path, files)?;
            } else if path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("al"))
            {
                files.push(path);
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    visit(root, &mut files)?;
    files.sort();
    files.into_iter().next().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("no .al source found under {}", root.display()),
        )
    })
}

async fn measure(
    backend: BuildBackend,
    scenario: &'static str,
    project: &Path,
    toolchain: &al_project::toolchain::AlToolchain,
    artifacts: &Path,
) -> Measurement {
    let backend_name = match backend {
        BuildBackend::Native => "native",
        BuildBackend::Alc => "alc",
    };
    let package_cache = project.join(".alpackages");
    let no_analyzers: Vec<String> = Vec::new();
    let load = load_average();
    let started = Instant::now();
    let result = al_compile::build(BuildRequest {
        project_root: project,
        backend,
        toolchain: Some(toolchain),
        dependency_packages: None,
        package_cache: Some(&package_cache),
        analyzers: Some(&no_analyzers),
        config: CompilationConfigOptions::default(),
    })
    .await;
    let elapsed_ns = elapsed_ns(started);

    match result {
        Ok(result) => {
            let app_size = result
                .app_path
                .as_deref()
                .and_then(|path| std::fs::metadata(path).ok())
                .map_or(0, |metadata| metadata.len());
            let artifact = result.app_path.as_deref().and_then(|source| {
                std::fs::create_dir_all(artifacts).ok()?;
                let destination = artifacts.join(format!("{backend_name}-{scenario}.app"));
                std::fs::copy(source, &destination).ok()?;
                Some(destination)
            });
            let errors = result
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
                .count();
            Measurement {
                backend: backend_name,
                scenario,
                elapsed_ns,
                load_average: load,
                success: result.success && app_size > 0,
                app_size,
                diagnostics: result.diagnostics.len(),
                errors,
                timings: result.timings,
                artifact,
                failure: (!result.success).then(|| result.output.chars().take(2_000).collect()),
            }
        }
        Err(error) => Measurement {
            backend: backend_name,
            scenario,
            elapsed_ns,
            load_average: load,
            success: false,
            app_size: 0,
            diagnostics: 0,
            errors: 0,
            timings: None,
            artifact: None,
            failure: Some(error.to_string()),
        },
    }
}

async fn run_pair(
    reverse: bool,
    scenario: &'static str,
    project: &Path,
    toolchain: &al_project::toolchain::AlToolchain,
    artifacts: &Path,
    output: &mut Vec<Measurement>,
) {
    let order = if reverse {
        [BuildBackend::Alc, BuildBackend::Native]
    } else {
        [BuildBackend::Native, BuildBackend::Alc]
    };
    for backend in order {
        output.push(measure(backend, scenario, project, toolchain, artifacts).await);
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let project = args
        .next()
        .map(PathBuf::from)
        .ok_or("usage: build_bench PROJECT ARTIFACT_DIR [--reverse]")?;
    let artifacts = args
        .next()
        .map(PathBuf::from)
        .ok_or("usage: build_bench PROJECT ARTIFACT_DIR [--reverse]")?;
    let reverse = args.any(|argument| argument == "--reverse");

    let project = std::fs::canonicalize(project)?;
    if !project.join("app.json").is_file() {
        return Err(format!("{} has no app.json", project.display()).into());
    }
    let toolchain = al_project::toolchain::find_toolchain()?;
    let mut measurements = Vec::with_capacity(6);

    run_pair(
        reverse,
        "processCold",
        &project,
        &toolchain,
        &artifacts,
        &mut measurements,
    )
    .await;
    run_pair(
        !reverse,
        "warmUnchanged",
        &project,
        &toolchain,
        &artifacts,
        &mut measurements,
    )
    .await;

    let edit_file = first_al_file(&project)?;
    let edit = EditGuard::apply(edit_file.clone())?;
    run_pair(
        reverse,
        "oneFileEdit",
        &project,
        &toolchain,
        &artifacts,
        &mut measurements,
    )
    .await;
    drop(edit);

    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "project": project,
            "editedFile": edit_file.strip_prefix(&project).unwrap_or(&edit_file),
            "toolchain": {
                "version": toolchain.version,
                "alc": toolchain.alc,
            },
            "measurements": measurements,
        }))?
    );
    Ok(())
}
