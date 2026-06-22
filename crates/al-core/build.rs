use std::path::PathBuf;
use std::process::Command;

fn main() {
    // The AL parser is now compiled by the standalone `tree-sitter-al` binding
    // crate (a normal `[dependencies]` entry), not here. This build script only
    // builds the optional .NET semantic bridge.
    build_semantic_bridge();
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

    println!("cargo:rerun-if-env-changed=AL_BRIDGE_PREBUILT");

    // If a prebuilt bridge directory is provided (e.g. cross-compilation, where
    // `dotnet` is unavailable inside the build container), copy it into OUT_DIR
    // instead of invoking `dotnet build`. The bridge is platform-agnostic IL,
    // so a host-built copy works for any target.
    if let Ok(prebuilt) = std::env::var("AL_BRIDGE_PREBUILT") {
        let prebuilt_dir = PathBuf::from(&prebuilt);
        let dll = prebuilt_dir.join("AlBridge.dll");
        if dll.is_file() {
            let _ = std::fs::create_dir_all(&output_dir);
            if let Ok(entries) = std::fs::read_dir(&prebuilt_dir) {
                for entry in entries.flatten() {
                    let from = entry.path();
                    if from.is_file() {
                        if let Some(name) = from.file_name() {
                            let _ = std::fs::copy(&from, output_dir.join(name));
                        }
                    }
                }
            }
            println!(
                "cargo:warning=Bridge DLL copied from AL_BRIDGE_PREBUILT={}",
                prebuilt
            );
            println!("cargo:rerun-if-changed=bridge/Bridge.cs");
            println!("cargo:rerun-if-changed=bridge/AlBridge.csproj");
            return;
        }
        println!(
            "cargo:warning=AL_BRIDGE_PREBUILT set to {prebuilt} but AlBridge.dll not found there; falling back to dotnet build"
        );
    }

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
