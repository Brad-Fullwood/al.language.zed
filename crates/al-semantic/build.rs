use std::path::PathBuf;
use std::process::Command;

fn main() {
    if std::env::var_os("CARGO_FEATURE_SEMANTIC").is_some() {
        build_semantic_bridge();
    }
}

/// Compile the C# semantic bridge DLL via `dotnet build`.
///
/// The bridge DLL is placed in OUT_DIR/bridge/ and found at runtime by
/// `host` via the baked-in OUT_DIR path.
fn build_semantic_bridge() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let bridge_dir = manifest_dir.join("bridge");
    let csproj = bridge_dir.join("AlBridge.csproj");

    assert!(
        csproj.is_file(),
        "bridge project not found at {}",
        csproj.display()
    );

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
        let runtime_config = prebuilt_dir.join("AlBridge.runtimeconfig.json");
        assert!(
            dll.is_file() && runtime_config.is_file(),
            "AL_BRIDGE_PREBUILT must contain AlBridge.dll and AlBridge.runtimeconfig.json"
        );
        std::fs::create_dir_all(&output_dir).expect("create semantic bridge output directory");
        for entry in std::fs::read_dir(&prebuilt_dir).expect("read AL_BRIDGE_PREBUILT") {
            let from = entry.expect("read AL_BRIDGE_PREBUILT entry").path();
            if from.is_file() {
                if let Some(name) = from.file_name() {
                    std::fs::copy(&from, output_dir.join(name))
                        .expect("copy prebuilt semantic bridge artifact");
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

    let status = Command::new("dotnet")
        .args(["build", "-c", "Release", "--nologo", "-v", "q", "-o"])
        .arg(&output_dir)
        .arg(&csproj)
        .status();

    match status {
        Ok(s) if s.success() => {
            assert!(
                output_dir.join("AlBridge.dll").is_file()
                    && output_dir.join("AlBridge.runtimeconfig.json").is_file(),
                "dotnet build succeeded but did not produce the required bridge artifacts"
            );
            println!(
                "cargo:warning=Bridge DLL compiled to {}",
                output_dir.display()
            );
        }
        Ok(s) => panic!(
            "dotnet build exited with {s} while compiling {}",
            csproj.display()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => panic!(
            "the `semantic` feature needs the .NET 8 SDK to build the AL bridge, \
             but `dotnet` was not found on PATH.\n\
             Install it from https://dotnet.microsoft.com/download/dotnet/8.0, or \
             point AL_BRIDGE_PREBUILT at a directory containing a prebuilt \
             AlBridge.dll + AlBridge.runtimeconfig.json.\n\
             Building without `--features semantic` links the no-op stub host instead."
        ),
        Err(e) => panic!("failed to run dotnet build: {e}"),
    }

    println!("cargo:rerun-if-changed=bridge/Bridge.cs");
    println!("cargo:rerun-if-changed=bridge/AlBridge.csproj");
}
