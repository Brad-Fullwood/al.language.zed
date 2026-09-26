//! Time the dependency source index and the call graph in one process.
//!
//! Loads every `.app` in a package folder, builds the dependency source
//! index and then the call graph, and prints the time of each step, the
//! summary memory the index reports and the process's peak resident memory.
//! The daemon runs the same steps at startup, but its numbers mix in the
//! request that triggered them and its other work.
//!
//! With a second argument the index reads and writes package summaries in
//! that directory, so a first run fills it and a second run measures the
//! load from disk:
//!
//! ```text
//! cargo run --release -p al-workspace --example dep_profile -- <.alpackages dir> [cache dir]
//! ```

use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let Some(package_dir) = args.next().map(PathBuf::from) else {
        eprintln!("usage: dep_profile <.alpackages dir> [cache dir]");
        std::process::exit(2);
    };
    let cache_dir = args.next().map(PathBuf::from);

    let mut paths: Vec<PathBuf> = std::fs::read_dir(&package_dir)
        .expect("read the package folder")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "app"))
        .collect();
    paths.sort();

    let workspace = al_workspace::Workspace::new();
    if let Some(dir) = cache_dir {
        workspace.enable_source_summary_cache(al_workspace::SourceSummaryCache::at(dir));
    }

    let started = Instant::now();
    workspace
        .symbols
        .load_packages(&paths)
        .expect("load packages");
    println!("package symbols: {:.2?}", started.elapsed());

    let started = Instant::now();
    let index = workspace
        .get_or_build_dependency_source_index()
        .expect("dependency source index");
    let progress = workspace.dependency_source_progress();
    println!(
        "dependency source index: {:.2?}, {} files, {} of {} packages from disk, {:?}",
        started.elapsed(),
        index.len(),
        progress.packages_from_disk,
        progress.packages_total,
        index.memory_stats(),
    );

    let started = Instant::now();
    let (_, call_graph) = workspace.get_or_build_call_graph().expect("call graph");
    println!(
        "call graph: {:.2?}, {} edges",
        started.elapsed(),
        call_graph.as_ref().map_or(0, |graph| graph.edge_count()),
    );

    if let Some(peak) = peak_resident_kib() {
        println!("peak resident memory: {} MB", peak / 1024);
    }
}

/// `VmHWM` from `/proc/self/status`, on Linux.
fn peak_resident_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|rest| rest.trim().trim_end_matches("kB").trim().parse().ok())
}
