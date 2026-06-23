//! T701: Symbol Index Performance Audit
//!
//! Profiles al-symbols loading and querying against real .app packages.
//! Run with: cargo test -p al-symbols --test perf_audit -- --nocapture

use std::path::PathBuf;
use std::time::Instant;

use al_lsp::symbols::{ObjectKind, SymbolIndex};

fn collect_app_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // Primary: symbol cache
    if let Some(cache) = dirs::cache_dir() {
        let pkg_dir = cache.join("al-lsp/packages");
        if pkg_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&pkg_dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.extension().and_then(|e| e.to_str()) == Some("app") {
                        paths.push(p);
                    }
                }
            }
        }
    }

    // Secondary: additional project packages (via AL_TEST_PACKAGES_PATH env var)
    let secondary = std::env::var("AL_TEST_PACKAGES_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/nonexistent"));
    if secondary.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&secondary) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().and_then(|e| e.to_str()) == Some("app") {
                    // Avoid duplicates (same package name, different version)
                    let name = p.file_name().unwrap().to_string_lossy().to_string();
                    if !paths.iter().any(|existing| {
                        existing
                            .file_name()
                            .unwrap()
                            .to_string_lossy()
                            .starts_with(name.split('_').next().unwrap_or(""))
                            && existing
                                .file_name()
                                .unwrap()
                                .to_string_lossy()
                                .contains(name.split('_').nth(1).unwrap_or(""))
                    }) {
                        paths.push(p);
                    }
                }
            }
        }
    }

    paths.sort();
    paths
}

#[test]
fn perf_audit_index_build() {
    let paths = collect_app_paths();
    if paths.is_empty() {
        eprintln!("SKIP: No .app files found for profiling");
        return;
    }

    eprintln!("\n=== T701: Symbol Index Performance Audit ===\n");
    eprintln!("Found {} .app packages:", paths.len());
    let mut total_bytes: u64 = 0;
    for p in &paths {
        let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        total_bytes += size;
        eprintln!(
            "  {:>8} {}",
            format_bytes(size),
            p.file_name().unwrap().to_string_lossy()
        );
    }
    eprintln!("  Total: {}", format_bytes(total_bytes));

    eprintln!("\n--- Phase 1: Index Build Time ---");

    let mut per_pkg_times = Vec::new();
    for p in &paths {
        let index = SymbolIndex::new();
        let t0 = Instant::now();
        let pkgs = index.load_packages(&[p]);
        let elapsed = t0.elapsed();
        let obj_count: usize = pkgs.iter().map(|pkg| pkg.objects.len()).sum();
        per_pkg_times.push((
            p.file_name().unwrap().to_string_lossy().to_string(),
            elapsed,
            obj_count,
        ));
    }

    per_pkg_times.sort_by_key(|p| std::cmp::Reverse(p.1)); // Slowest first
    for (name, dur, objs) in &per_pkg_times {
        eprintln!("  {:>8.2?}  {:>5} objects  {}", dur, objs, name);
    }

    let index = SymbolIndex::new();
    let t0 = Instant::now();
    let all_pkgs = index.load_packages(&paths);
    let build_time = t0.elapsed();
    let total_objects: usize = all_pkgs.iter().map(|p| p.objects.len()).sum();

    eprintln!(
        "\n  Full build: {:?} ({} objects)",
        build_time, total_objects
    );
    eprintln!("  Index entries: {}", index.len());

    eprintln!("\n--- Phase 2: Memory Estimation ---");

    let mut total_methods = 0usize;
    let mut total_fields = 0usize;
    let mut total_controls = 0usize;
    let mut total_enum_values = 0usize;
    let mut total_keys = 0usize;
    let mut total_properties = 0usize;
    let mut total_variables = 0usize;
    let mut total_params = 0usize;
    let mut total_attributes = 0usize;
    let mut total_string_bytes = 0usize;

    for pkg in &all_pkgs {
        for obj in &pkg.objects {
            total_string_bytes += obj.name.len() + obj.package.len();
            if let Some(ref ext) = obj.extends {
                total_string_bytes += ext.len();
            }
            for m in &obj.methods {
                total_methods += 1;
                total_string_bytes += m.name.len();
                if let Some(ref r) = m.return_type {
                    total_string_bytes += r.len();
                }
                for p in &m.parameters {
                    total_params += 1;
                    total_string_bytes += p.name.len() + p.type_name.len();
                }
                for a in &m.attributes {
                    total_attributes += 1;
                    total_string_bytes += a.name.len();
                    for arg in &a.arguments {
                        total_string_bytes += arg.len();
                    }
                }
            }
            for f in &obj.fields {
                total_fields += 1;
                total_string_bytes += f.name.len() + f.type_name.len();
                for p in &f.properties {
                    total_properties += 1;
                    total_string_bytes += p.name.len() + p.value.len();
                }
            }
            for c in &obj.controls {
                total_controls += count_controls(c);
            }
            total_enum_values += obj.enum_values.len();
            total_keys += obj.keys.len();
            total_variables += obj.variables.len();
        }
    }

    eprintln!("  Objects:     {:>7}", total_objects);
    eprintln!("  Methods:     {:>7}", total_methods);
    eprintln!("  Parameters:  {:>7}", total_params);
    eprintln!("  Attributes:  {:>7}", total_attributes);
    eprintln!("  Fields:      {:>7}", total_fields);
    eprintln!("  Controls:    {:>7}", total_controls);
    eprintln!("  Enum values: {:>7}", total_enum_values);
    eprintln!("  Keys:        {:>7}", total_keys);
    eprintln!("  Properties:  {:>7}", total_properties);
    eprintln!("  Variables:   {:>7}", total_variables);
    eprintln!(
        "  String data: {:>7}",
        format_bytes(total_string_bytes as u64)
    );

    // Rough memory estimate: each SymbolEntry is ~200 bytes + strings + sub-elements
    // Arc overhead: 16 bytes per arc. DashMap entry overhead: ~64 bytes.
    // Each entry is indexed 4 times (by_name, by_kind_id, by_kind, all) = 4 Arcs
    let est_entry_overhead = total_objects * (200 + 4 * 16 + 4 * 64);
    let est_method_bytes = total_methods * 120;
    let est_field_bytes = total_fields * 80;
    let est_control_bytes = total_controls * 60;
    let est_total = est_entry_overhead
        + est_method_bytes
        + est_field_bytes
        + est_control_bytes
        + total_string_bytes;
    eprintln!(
        "  Estimated index memory: ~{}",
        format_bytes(est_total as u64)
    );

    eprintln!("\n--- Phase 3: Query Latency ---");

    let queries = [
        "Customer",
        "Sales",
        "Post",
        "Gen. Journal",
        "Vendor",
        "Item",
    ];
    eprintln!("\n  search(query, limit=100):");
    for q in &queries {
        let t0 = Instant::now();
        let results = index.search(q, 100);
        let dur = t0.elapsed();
        eprintln!("    {:>8.2?}  {:>4} results  \"{}\"", dur, results.len(), q);
    }

    eprintln!("\n  search(\"\", limit):");
    for limit in [10, 100, 1000, 5000] {
        let t0 = Instant::now();
        let results = index.search("", limit);
        let dur = t0.elapsed();
        eprintln!(
            "    {:>8.2?}  {:>5} results  limit={}",
            dur,
            results.len(),
            limit
        );
    }

    eprintln!("\n  get_by_name():");
    let name_queries = [
        "Customer",
        "Sales Header",
        "Gen. Journal Line",
        "G/L Entry",
        "Item",
    ];
    for q in &name_queries {
        let t0 = Instant::now();
        let results = index.get_by_name(q);
        let dur = t0.elapsed();
        eprintln!("    {:>8.2?}  {:>3} results  \"{}\"", dur, results.len(), q);
    }

    eprintln!("\n  get_by_id():");
    let id_queries = [
        (ObjectKind::Table, 18),    // Customer
        (ObjectKind::Table, 36),    // Sales Header
        (ObjectKind::Codeunit, 80), // Sales-Post
        (ObjectKind::Page, 22),     // Customer List
        (ObjectKind::Table, 9999),  // Nonexistent
    ];
    for (kind, id) in &id_queries {
        let t0 = Instant::now();
        let results = index.get_by_id(*kind, *id);
        let dur = t0.elapsed();
        eprintln!(
            "    {:>8.2?}  {:>3} results  {:?} {}",
            dur,
            results.len(),
            kind,
            id
        );
    }

    eprintln!("\n  get_events():");
    let event_queries = ["Post", "Release", "Validate", "Insert", ""];
    for q in &event_queries {
        let t0 = Instant::now();
        let results = index.get_events(q);
        let dur = t0.elapsed();
        eprintln!(
            "    {:>8.2?}  {:>4}p {:>4}s  \"{}\"",
            dur,
            results.publishers.len(),
            results.subscribers.len(),
            q
        );
    }

    eprintln!("\n  get_composed():");
    let compose_queries = [
        (ObjectKind::Table, "Customer"),
        (ObjectKind::Table, "Sales Header"),
        (ObjectKind::Page, "Customer Card"),
        (ObjectKind::Enum, "Sales Document Type"),
        (ObjectKind::Table, "Nonexistent"),
    ];
    for (kind, name) in &compose_queries {
        let t0 = Instant::now();
        let result = index.get_composed_cached(*kind, name);
        let dur = t0.elapsed();
        let info = match &result {
            Some(c) => format!(
                "{}f {}m {}ext",
                c.all_fields.len(),
                c.all_methods.len(),
                c.extensions.len()
            ),
            None => "not found".to_string(),
        };
        eprintln!("    {:>8.2?}  {}  {:?} \"{}\"", dur, info, kind, name);
    }

    eprintln!("\n  get_extensions_of():");
    let ext_queries = ["Customer", "Sales Header", "Item", "G/L Entry"];
    for q in &ext_queries {
        let t0 = Instant::now();
        let results = index.get_extensions_of(q);
        let dur = t0.elapsed();
        eprintln!(
            "    {:>8.2?}  {:>3} extensions  \"{}\"",
            dur,
            results.len(),
            q
        );
    }

    eprintln!("\n  get_by_kind():");
    let kinds = [
        ObjectKind::Table,
        ObjectKind::Page,
        ObjectKind::Codeunit,
        ObjectKind::Enum,
        ObjectKind::TableExtension,
    ];
    for kind in &kinds {
        let t0 = Instant::now();
        let results = index.get_by_kind(*kind);
        let dur = t0.elapsed();
        eprintln!(
            "    {:>8.2?}  {:>5} objects  {:?}",
            dur,
            results.len(),
            kind
        );
    }

    eprintln!("\n--- Phase 4: Bottleneck Analysis ---");

    // Measure the hot path: search("", usize::MAX) used by get_events()
    let t0 = Instant::now();
    let all_entries = index.search("", usize::MAX);
    let full_scan_time = t0.elapsed();
    eprintln!(
        "  Full scan via search(\"\", MAX): {:?} ({} entries)",
        full_scan_time,
        all_entries.len()
    );

    eprintln!("\n  DashMap shard analysis:");
    eprintln!("    by_name entries:    {}", index.len());
    // The 'all' map is the hot path for search/events

    eprintln!("\n  Rebuild consistency (3 runs):");
    for i in 0..3 {
        let idx = SymbolIndex::new();
        let t0 = Instant::now();
        idx.load_packages(&paths);
        let dur = t0.elapsed();
        eprintln!("    Run {}: {:?} ({} entries)", i + 1, dur, idx.len());
    }

    eprintln!("\n=== End Performance Audit ===\n");
}

fn count_controls(control: &al_lsp::symbols::ControlSymbol) -> usize {
    1 + control.children.iter().map(count_controls).sum::<usize>()
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}
