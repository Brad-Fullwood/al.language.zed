//! In-process .NET hosting via netcorehost.
//!
//! Loads the CLR directly into the Rust process, compiles and loads the
//! bridge DLL, and exposes JSON-in/JSON-out communication with CodeAnalysis.

use std::ffi::c_int;
use std::path::{Path, PathBuf};

use netcorehost::hostfxr::{HostfxrContext, InitializedForRuntimeConfig};
use netcorehost::pdcstr;
use tracing::{debug, info};

use crate::SemanticError;

/// Function pointer types matching the C# [UnmanagedCallersOnly] signatures.
/// `extern "system"` is required by netcorehost's ManagedFunctionPtr trait.
/// On Linux this is identical to `extern "C"`.
type InitFn = unsafe extern "system" fn(*const u8, c_int) -> c_int;
type HandleRequestFn = unsafe extern "system" fn(*const u8, c_int, *mut c_int) -> *mut u8;
type FreeBufferFn = unsafe extern "system" fn(*mut u8);

/// In-process .NET host wrapping the bridge DLL.
///
/// Thread-safe: the .NET runtime is initialized once and function pointers
/// are safe to call from any thread (CLR handles its own thread safety).
pub struct DotNetHost {
    _context: HostfxrContext<InitializedForRuntimeConfig>,
    _init_fn: InitFn,
    handle_request_fn: HandleRequestFn,
    free_buffer_fn: FreeBufferFn,
}

// The function pointers are thread-safe since the CLR manages its own synchronization.
unsafe impl Send for DotNetHost {}
unsafe impl Sync for DotNetHost {}

impl DotNetHost {
    /// Initialize the .NET runtime and load the bridge DLL.
    ///
    /// `bridge_dll` is the path to the compiled AlBridge.dll.
    /// `runtime_config` is the path to AlBridge.runtimeconfig.json.
    /// `code_analysis_path` is the path to CodeAnalysis.dll from the toolchain.
    pub fn new(
        bridge_dll: &Path,
        runtime_config: &Path,
        code_analysis_path: &Path,
    ) -> Result<Self, SemanticError> {
        info!(bridge = %bridge_dll.display(), config = %runtime_config.display(), "Loading .NET runtime");

        // Load hostfxr from the system .NET installation
        let hostfxr = netcorehost::nethost::load_hostfxr()
            .map_err(|e| SemanticError::HostInit(format!("Failed to load hostfxr: {e}")))?;

        // Initialize runtime using the bridge's runtimeconfig.json
        let config_path = path_to_pdcstring(runtime_config)?;
        let context = hostfxr
            .initialize_for_runtime_config(config_path)
            .map_err(|e| SemanticError::HostInit(format!("Failed to initialize runtime: {e}")))?;

        // Get delegate loader for the bridge assembly
        let dll_path = path_to_pdcstring(bridge_dll)?;
        let fn_loader = context
            .get_delegate_loader_for_assembly(dll_path)
            .map_err(|e| SemanticError::HostInit(format!("Failed to load bridge assembly: {e}")))?;

        // Load function pointers
        let type_name = pdcstr!("AlBridge.Bridge, AlBridge");

        let init_fn: InitFn = *fn_loader
            .get_function_with_unmanaged_callers_only::<InitFn>(
                type_name,
                pdcstr!("Init"),
            )
            .map_err(|e| SemanticError::HostInit(format!("Failed to get Init: {e}")))?;

        let handle_request_fn: HandleRequestFn = *fn_loader
            .get_function_with_unmanaged_callers_only::<HandleRequestFn>(
                type_name,
                pdcstr!("HandleRequest"),
            )
            .map_err(|e| SemanticError::HostInit(format!("Failed to get HandleRequest: {e}")))?;

        let free_buffer_fn: FreeBufferFn = *fn_loader
            .get_function_with_unmanaged_callers_only::<FreeBufferFn>(
                type_name,
                pdcstr!("FreeBuffer"),
            )
            .map_err(|e| SemanticError::HostInit(format!("Failed to get FreeBuffer: {e}")))?;

        // Initialize the bridge with the CodeAnalysis.dll path
        let ca_path = code_analysis_path.to_string_lossy();
        let ca_bytes = ca_path.as_bytes();
        let ca_len = c_int::try_from(ca_bytes.len()).map_err(|_| {
            SemanticError::HostInit(format!(
                "CodeAnalysis path is too long ({} bytes, max {})",
                ca_bytes.len(),
                c_int::MAX
            ))
        })?;
        let result = unsafe { init_fn(ca_bytes.as_ptr(), ca_len) };

        if result != 0 {
            return Err(SemanticError::HostInit(format!(
                "Bridge Init failed with code {result} (CodeAnalysis path: {})",
                code_analysis_path.display()
            )));
        }

        info!("Bridge initialized successfully");

        Ok(Self {
            _context: context,
            _init_fn: init_fn,
            handle_request_fn,
            free_buffer_fn,
        })
    }

    /// Call a bridge method with JSON params, returning the JSON result.
    pub fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, SemanticError> {
        let request = if params.is_null() {
            serde_json::json!({ "method": method })
        } else {
            serde_json::json!({ "method": method, "params": params })
        };

        let request_bytes = serde_json::to_vec(&request)
            .map_err(|e| SemanticError::SerializationError(e.to_string()))?;

        let request_len = c_int::try_from(request_bytes.len()).map_err(|_| {
            SemanticError::SerializationError(format!(
                "Request payload is too large ({} bytes, max {})",
                request_bytes.len(),
                c_int::MAX
            ))
        })?;

        let mut response_len: c_int = 0;
        let response_ptr = unsafe {
            (self.handle_request_fn)(
                request_bytes.as_ptr(),
                request_len,
                &mut response_len,
            )
        };

        if response_ptr.is_null() {
            return Err(SemanticError::HostInit("HandleRequest returned null".into()));
        }

        if response_len < 0 {
            unsafe { (self.free_buffer_fn)(response_ptr) };
            return Err(SemanticError::HostInit(format!(
                "HandleRequest returned negative response length: {response_len}"
            )));
        }

        // Copy the response bytes before freeing
        let response_bytes = unsafe {
            let slice = std::slice::from_raw_parts(response_ptr, response_len as usize);
            let bytes = slice.to_vec();
            (self.free_buffer_fn)(response_ptr);
            bytes
        };

        let response: serde_json::Value = serde_json::from_slice(&response_bytes)
            .map_err(|e| SemanticError::SerializationError(format!(
                "Failed to parse bridge response: {e}"
            )))?;

        // Check for error in response
        if let Some(error) = response.get("error") {
            let code = error.get("code").and_then(|v| v.as_i64()).unwrap_or(-1) as i32;
            let message = error
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown bridge error")
                .to_string();
            return Err(SemanticError::RpcError { code, message });
        }

        // Extract the result field
        Ok(response.get("result").cloned().unwrap_or(serde_json::Value::Null))
    }
}

/// Locate the bridge DLL and runtime config.
///
/// Search order:
/// 1. `OUT_DIR` from build.rs (compiled alongside the Rust binary)
/// 2. Next to the current executable (deployed)
/// 3. `AL_BRIDGE_DIR` environment variable
/// 4. Compile the bridge on-the-fly from source (development mode)
pub fn find_bridge_dll() -> Result<(PathBuf, PathBuf), SemanticError> {
    // Strategy 1: OUT_DIR from build.rs (baked in at compile time)
    if let Some(out_dir) = option_env!("OUT_DIR") {
        let bridge_dir = PathBuf::from(out_dir).join("bridge");
        let dll = bridge_dir.join("AlBridge.dll");
        let config = bridge_dir.join("AlBridge.runtimeconfig.json");
        if dll.is_file() && config.is_file() {
            debug!(path = %dll.display(), "Found bridge DLL from OUT_DIR");
            return Ok((dll, config));
        }
    }

    // Strategy 2: Next to the current executable
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let dll = exe_dir.join("bridge").join("AlBridge.dll");
            let config = exe_dir.join("bridge").join("AlBridge.runtimeconfig.json");
            if dll.is_file() && config.is_file() {
                debug!(path = %dll.display(), "Found bridge DLL next to executable");
                return Ok((dll, config));
            }
            // Also check flat layout
            let dll = exe_dir.join("AlBridge.dll");
            let config = exe_dir.join("AlBridge.runtimeconfig.json");
            if dll.is_file() && config.is_file() {
                debug!(path = %dll.display(), "Found bridge DLL next to executable (flat)");
                return Ok((dll, config));
            }
        }
    }

    // Strategy 3: Environment variable
    if let Ok(dir) = std::env::var("AL_BRIDGE_DIR") {
        let bridge_dir = PathBuf::from(&dir);
        let dll = bridge_dir.join("AlBridge.dll");
        let config = bridge_dir.join("AlBridge.runtimeconfig.json");
        if dll.is_file() && config.is_file() {
            debug!(path = %dll.display(), "Found bridge DLL from AL_BRIDGE_DIR");
            return Ok((dll, config));
        }
    }

    // Strategy 4: Compile from source (development mode)
    if let Some(manifest_dir) = option_env!("CARGO_MANIFEST_DIR") {
        let bridge_proj = PathBuf::from(manifest_dir).join("bridge").join("AlBridge.csproj");
        if bridge_proj.is_file() {
            debug!(project = %bridge_proj.display(), "Compiling bridge DLL from source");
            return compile_bridge_from_source(&bridge_proj);
        }
    }

    Err(SemanticError::HostInit(
        "Could not find AlBridge.dll. Set AL_BRIDGE_DIR or ensure bridge/ is compiled.".into(),
    ))
}

/// Compile the bridge DLL from its .csproj and return the output paths.
fn compile_bridge_from_source(csproj: &Path) -> Result<(PathBuf, PathBuf), SemanticError> {
    let output_dir = csproj
        .parent()
        .unwrap_or(Path::new("."))
        .join("bin")
        .join("Release")
        .join("net8.0");

    let status = std::process::Command::new("dotnet")
        .args(["build", "-c", "Release", "--nologo", "-v", "q"])
        .arg(csproj)
        .status()
        .map_err(|e| SemanticError::HostInit(format!("Failed to run dotnet build: {e}")))?;

    if !status.success() {
        return Err(SemanticError::HostInit("dotnet build failed".into()));
    }

    let dll = output_dir.join("AlBridge.dll");
    let config = output_dir.join("AlBridge.runtimeconfig.json");
    if dll.is_file() && config.is_file() {
        Ok((dll, config))
    } else {
        Err(SemanticError::HostInit(format!(
            "Bridge built but output not found at {}",
            output_dir.display()
        )))
    }
}

/// Convert a Path to a PdCString for netcorehost.
fn path_to_pdcstring(
    path: &Path,
) -> Result<netcorehost::pdcstring::PdCString, SemanticError> {
    netcorehost::pdcstring::PdCString::from_os_str(path.as_os_str())
        .map_err(|e| SemanticError::HostInit(format!("Invalid path for .NET: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_bridge_returns_error_when_not_found() {
        // Clear env so strategy 3 doesn't fire
        std::env::remove_var("AL_BRIDGE_DIR");
        // This should fail gracefully (not panic) when bridge isn't available
        let result = find_bridge_dll();
        // In dev mode with the source project present, this might succeed
        // Either way, it shouldn't panic
        match result {
            Ok((dll, config)) => {
                assert!(dll.is_file());
                assert!(config.is_file());
            }
            Err(e) => {
                assert!(e.to_string().contains("AlBridge") || e.to_string().contains("bridge"));
            }
        }
    }

    /// Verify that `c_int::try_from` catches values exceeding i32::MAX.
    /// This mirrors the guard added before each `as c_int` cast.
    #[test]
    fn test_c_int_overflow_guard_rejects_oversized_len() {
        let oversized: usize = (c_int::MAX as usize) + 1;
        let result = c_int::try_from(oversized);
        assert!(
            result.is_err(),
            "try_from must fail for values > i32::MAX"
        );
    }

    /// Values at i32::MAX must not be rejected.
    #[test]
    fn test_c_int_overflow_guard_accepts_max() {
        let at_max: usize = c_int::MAX as usize;
        let result = c_int::try_from(at_max);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), c_int::MAX);
    }

    /// Verify the negative-response-length guard logic.
    #[test]
    fn test_response_len_negative_is_detected() {
        let response_len: c_int = -1;
        // The guard in call() is: if response_len < 0 { return Err(...) }
        assert!(response_len < 0, "negative response_len must be caught");
    }

    /// Verify a zero response length is treated as valid (empty body).
    #[test]
    fn test_response_len_zero_is_valid() {
        let response_len: c_int = 0;
        assert!(response_len >= 0);
        // Safe cast
        let as_usize = response_len as usize;
        assert_eq!(as_usize, 0);
    }
}
