use std::path::PathBuf;
use std::process::Command;

fn main() {
    build_tree_sitter_parser();
    build_semantic_bridge();
}

fn build_tree_sitter_parser() {
    let src_dir = std::path::Path::new("../../tree-sitter-al/src");

    let mut build = cc::Build::new();
    build.include(src_dir).file(src_dir.join("parser.c"));

    let scanner = src_dir.join("scanner.c");
    if scanner.exists() {
        build.file(scanner);
    }

    // Silence warnings from tree-sitter's generated parser.c — we don't control its output.
    build.warnings(false).flag_if_supported("-w");

    build.compile("tree-sitter-al");
}

/// Compile the C# semantic bridge DLL via `dotnet build`.
///
/// The bridge DLL is placed in OUT_DIR/bridge/ and found at runtime by
/// `semantic::host` via the baked-in OUT_DIR path. Skipped if dotnet or the
/// project file are absent (CI hosts without .NET).
fn build_semantic_bridge() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let bridge_dir = manifest_dir.join("bridge");
    let csproj = bridge_dir.join("AlBridge.csproj");

    if !csproj.is_file() {
        println!(
            "cargo:warning=Bridge project not found at {}, skipping .NET build",
            csproj.display()
        );
        return;
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let output_dir = out_dir.join("bridge");

    let status = Command::new("dotnet")
        .args(["build", "-c", "Release", "--nologo", "-v", "q", "-o"])
        .arg(&output_dir)
        .arg(&csproj)
        .status();

    match status {
        Ok(s) if s.success() => {
            println!(
                "cargo:warning=Bridge DLL compiled to {}",
                output_dir.display()
            );
        }
        Ok(s) => {
            println!("cargo:warning=dotnet build exited with {s}, bridge DLL may not be available");
        }
        Err(e) => {
            println!("cargo:warning=dotnet not found ({e}), bridge DLL will not be compiled");
        }
    }

    println!("cargo:rerun-if-changed=bridge/Bridge.cs");
    println!("cargo:rerun-if-changed=bridge/AlBridge.csproj");
}
