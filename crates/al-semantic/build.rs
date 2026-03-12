//! Build script: compile the C# bridge DLL via `dotnet build`.
//!
//! The bridge DLL is placed in OUT_DIR/bridge/ and found at runtime
//! by host.rs via the baked-in OUT_DIR path.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let bridge_dir = manifest_dir.join("bridge");
    let csproj = bridge_dir.join("AlBridge.csproj");

    // Only compile if the bridge project exists (skip in CI without .NET)
    if !csproj.is_file() {
        println!("cargo:warning=Bridge project not found at {}, skipping .NET build", csproj.display());
        return;
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let output_dir = out_dir.join("bridge");

    let status = Command::new("dotnet")
        .args([
            "build",
            "-c", "Release",
            "--nologo",
            "-v", "q",
            "-o",
        ])
        .arg(&output_dir)
        .arg(&csproj)
        .status();

    match status {
        Ok(s) if s.success() => {
            println!("cargo:warning=Bridge DLL compiled to {}", output_dir.display());
        }
        Ok(s) => {
            println!("cargo:warning=dotnet build exited with {s}, bridge DLL may not be available");
        }
        Err(e) => {
            println!("cargo:warning=dotnet not found ({e}), bridge DLL will not be compiled");
        }
    }

    // Rebuild when bridge source changes
    println!("cargo:rerun-if-changed=bridge/Bridge.cs");
    println!("cargo:rerun-if-changed=bridge/AlBridge.csproj");
}
