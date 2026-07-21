//! In-process .NET hosting via netcorehost.
//!
//! Loads the CLR directly into the Rust process, compiles and loads the
//! bridge DLL, and exposes JSON-in/JSON-out communication with CodeAnalysis.
//!
//! When the `semantic` Cargo feature is disabled, host construction returns
//! [`SemanticError::NotInitialized`].

#[cfg(feature = "semantic")]
use std::ffi::c_int;
use std::path::{Path, PathBuf};

#[cfg(feature = "semantic")]
use netcorehost::hostfxr::{HostfxrContext, InitializedForRuntimeConfig};
#[cfg(feature = "semantic")]
use netcorehost::pdcstr;
use tracing::debug;
#[cfg(feature = "semantic")]
use tracing::info;

use super::SemanticError;

#[cfg(feature = "semantic")]
type InitFn = unsafe extern "system" fn(*const u8, c_int) -> c_int;
#[cfg(feature = "semantic")]
type HandleRequestFn = unsafe extern "system" fn(*const u8, c_int, *mut c_int) -> *mut u8;
#[cfg(feature = "semantic")]
type FreeBufferFn = unsafe extern "system" fn(*mut u8);
#[cfg(feature = "semantic")]
type GetLastErrorFn = unsafe extern "system" fn(*mut c_int) -> *mut u8;

/// Defensive upper bound on a single bridge response, in bytes. A buggy or
/// misbehaving bridge could report a `response_len` larger than the buffer it
/// actually allocated; this cap prevents `from_raw_parts` from constructing an
/// out-of-bounds slice. No legitimate CodeAnalysis response approaches this
/// size. This is a resource/safety limit, not an AL language value.
#[cfg(feature = "semantic")]
const MAX_RESPONSE_BYTES: usize = 256 * 1024 * 1024;

/// Mirrors the managed-side request cap. Document text is limited to 16 MiB;
/// the extra headroom covers JSON escaping, paths, and analyzer names without
/// allowing an arbitrary multi-gigabyte allocation at the FFI boundary.
#[cfg(feature = "semantic")]
const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;

/// In-process .NET host wrapping the bridge DLL.
///
/// Thread-safe: the .NET runtime is initialized once and function pointers
/// are safe to call from any thread (CLR handles its own thread safety).
#[cfg(feature = "semantic")]
pub(crate) struct DotNetHost {
    _context: HostfxrContext<InitializedForRuntimeConfig>,
    _init_fn: InitFn,
    handle_request_fn: HandleRequestFn,
    free_buffer_fn: FreeBufferFn,
}

/// Disabled-feature implementation of the host.
#[cfg(not(feature = "semantic"))]
pub(crate) struct DotNetHost;

// SAFETY: DotNetHost wraps CLR function pointers obtained from netcorehost.
// The pointers are valid for the lifetime of `_context` (declared first, dropped last).
// Concurrent calls are serialized by std::sync::Mutex in SemanticBridge::call().
#[cfg(feature = "semantic")]
unsafe impl Send for DotNetHost {}
#[cfg(feature = "semantic")]
unsafe impl Sync for DotNetHost {}

#[cfg(feature = "semantic")]
impl DotNetHost {
    pub fn new(
        bridge_dll: &Path,
        runtime_config: &Path,
        code_analysis_path: &Path,
    ) -> Result<Self, SemanticError> {
        info!(bridge = %bridge_dll.display(), config = %runtime_config.display(), "Loading .NET runtime");

        let hostfxr = netcorehost::nethost::load_hostfxr()
            .map_err(|e| SemanticError::HostInit(format!("Failed to load hostfxr: {e}")))?;

        let config_path = path_to_pdcstring(runtime_config)?;
        let context = hostfxr
            .initialize_for_runtime_config(config_path)
            .map_err(|e| SemanticError::HostInit(format!("Failed to initialize runtime: {e}")))?;

        let dll_path = path_to_pdcstring(bridge_dll)?;
        let fn_loader = context
            .get_delegate_loader_for_assembly(dll_path)
            .map_err(|e| SemanticError::HostInit(format!("Failed to load bridge assembly: {e}")))?;

        let type_name = pdcstr!("AlBridge.Bridge, AlBridge");

        let init_fn: InitFn = *fn_loader
            .get_function_with_unmanaged_callers_only::<InitFn>(type_name, pdcstr!("Init"))
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

        let get_last_error_fn: GetLastErrorFn = *fn_loader
            .get_function_with_unmanaged_callers_only::<GetLastErrorFn>(
                type_name,
                pdcstr!("GetLastError"),
            )
            .map_err(|e| SemanticError::HostInit(format!("Failed to get GetLastError: {e}")))?;

        let ca_path = code_analysis_path.to_str().ok_or_else(|| {
            SemanticError::HostInit(format!(
                "CodeAnalysis path is not valid UTF-8: {}",
                code_analysis_path.display()
            ))
        })?;
        let ca_bytes = ca_path.as_bytes();
        let ca_len = c_int::try_from(ca_bytes.len()).map_err(|_| {
            SemanticError::HostInit(format!(
                "CodeAnalysis path is too long ({} bytes, max {})",
                ca_bytes.len(),
                c_int::MAX
            ))
        })?;
        // SAFETY: ca_bytes is live for the duration of the call (owned by ca_path,
        // which is kept on the stack until init_fn returns). ca_len is computed
        // from ca_bytes.len() above, so the (ptr, len) pair is valid. init_fn is
        // a C ABI function pointer obtained from the CLR via get_function_pointer.
        let result = unsafe { init_fn(ca_bytes.as_ptr(), ca_len) };

        if result != 0 {
            let mut error_len: c_int = 0;
            // SAFETY: the delegate comes from the same loaded bridge assembly;
            // error_len is a valid out pointer. The returned allocation is
            // released with that assembly's FreeBuffer delegate below.
            let error_ptr = unsafe { get_last_error_fn(&mut error_len) };
            let detail = if !error_ptr.is_null() && (1..=1024 * 1024).contains(&error_len) {
                // SAFETY: GetLastError reports the exact allocation length and
                // keeps the buffer alive until FreeBuffer is called.
                let bytes =
                    unsafe { std::slice::from_raw_parts(error_ptr, error_len as usize).to_vec() };
                String::from_utf8(bytes)
                    .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
            } else {
                "bridge did not return initialization details".to_string()
            };
            if !error_ptr.is_null() {
                // SAFETY: error_ptr was allocated by GetLastError in this bridge.
                unsafe { free_buffer_fn(error_ptr) };
            }
            return Err(SemanticError::HostInit(format!(
                "Bridge Init failed with code {result}: {detail} (CodeAnalysis path: {})",
                code_analysis_path.display(),
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
    ///
    /// Takes `&mut self` because the CLR response buffer is shared and not
    /// safe for concurrent access. The caller (SemanticBridge) serializes
    /// calls via a `std::sync::Mutex<DotNetHost>`.
    pub fn call(
        &mut self,
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

        if request_bytes.len() > MAX_REQUEST_BYTES {
            return Err(SemanticError::SerializationError(format!(
                "Request payload is too large ({} bytes, max {MAX_REQUEST_BYTES})",
                request_bytes.len()
            )));
        }
        let request_len = c_int::try_from(request_bytes.len()).map_err(|_| {
            SemanticError::SerializationError(format!(
                "Request payload is too large ({} bytes, max {})",
                request_bytes.len(),
                c_int::MAX
            ))
        })?;

        let mut response_len: c_int = 0;
        // SAFETY: request_bytes is live for the duration of the call (owned by
        // the local Vec). request_len matches request_bytes.len(). response_len
        // is a unique mutable reference to a stack value. handle_request_fn is a
        // C ABI function pointer obtained from the CLR via get_function_pointer.
        let response_ptr = unsafe {
            (self.handle_request_fn)(request_bytes.as_ptr(), request_len, &mut response_len)
        };

        if response_ptr.is_null() {
            return Err(SemanticError::HostInit(
                "HandleRequest returned null".into(),
            ));
        }

        if response_len < 0 {
            // SAFETY: response_ptr is non-null (checked above). free_buffer_fn was
            // obtained from the same CLR load as handle_request_fn and frees the
            // same allocator's buffer.
            unsafe { (self.free_buffer_fn)(response_ptr) };
            return Err(SemanticError::HostInit(format!(
                "HandleRequest returned negative response length: {response_len}"
            )));
        }

        // Defensive upper bound on the reported response length. The buffer was
        // allocated by the bridge, but a buggy bridge could report a length far
        // larger than what it actually allocated; passing that to
        // `from_raw_parts` below would create an out-of-bounds slice and an OOB
        // read in `to_vec`. Cap at a generous limit (no legitimate bridge
        // response approaches it) and free + error out otherwise. Mirrors the
        // request-side `c_int::try_from` bound above.
        if response_len as usize > MAX_RESPONSE_BYTES {
            // SAFETY: response_ptr is non-null (checked above); free_buffer_fn
            // frees the buffer this CLR allocated.
            unsafe { (self.free_buffer_fn)(response_ptr) };
            return Err(SemanticError::HostInit(format!(
                "HandleRequest returned implausible response length: {response_len} bytes (max {MAX_RESPONSE_BYTES})"
            )));
        }

        // Copy the response bytes before freeing. ClrBuf ensures the buffer is
        // freed even if to_vec panics (e.g. on OOM), avoiding a leak.
        struct ClrBuf {
            ptr: *mut u8,
            free: FreeBufferFn,
        }
        impl Drop for ClrBuf {
            fn drop(&mut self) {
                if !self.ptr.is_null() {
                    // SAFETY: self.ptr was returned by handle_request_fn from the
                    // same CLR module that exported self.free; valid until freed.
                    unsafe { (self.free)(self.ptr) };
                    self.ptr = std::ptr::null_mut();
                }
            }
        }
        let _buf = ClrBuf {
            ptr: response_ptr,
            free: self.free_buffer_fn,
        };
        // SAFETY: response_ptr is non-null (checked above), response_len is
        // non-negative (checked), and the buffer is valid for response_len bytes
        // until ClrBuf::drop runs at the end of this scope.
        let response_bytes = unsafe {
            let slice = std::slice::from_raw_parts(response_ptr, response_len as usize);
            slice.to_vec()
        };

        let response: serde_json::Value = serde_json::from_slice(&response_bytes).map_err(|e| {
            SemanticError::SerializationError(format!("Failed to parse bridge response: {e}"))
        })?;

        let response_object = response.as_object().ok_or_else(|| {
            SemanticError::SerializationError("Bridge response root is not an object".to_string())
        })?;
        let result = response_object.get("result");
        let error = response_object.get("error");
        if result.is_some() == error.is_some() {
            return Err(SemanticError::SerializationError(
                "Bridge response must contain exactly one of 'result' or 'error'".to_string(),
            ));
        }

        if let Some(error) = error {
            let code = error
                .get("code")
                .and_then(|v| v.as_i64())
                .and_then(|v| i32::try_from(v).ok())
                .unwrap_or(-1);
            let message = error
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown bridge error")
                .to_string();
            return Err(SemanticError::RpcError { code, message });
        }

        Ok(result.cloned().unwrap_or(serde_json::Value::Null))
    }
}

#[cfg(not(feature = "semantic"))]
impl DotNetHost {
    pub fn new(
        _bridge_dll: &Path,
        _runtime_config: &Path,
        _code_analysis_path: &Path,
    ) -> Result<Self, SemanticError> {
        Err(SemanticError::NotInitialized)
    }

    pub fn call(
        &mut self,
        _method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, SemanticError> {
        Err(SemanticError::NotInitialized)
    }
}

fn check_bridge_pair(dll: PathBuf, config: PathBuf) -> Option<(PathBuf, PathBuf)> {
    if dll.is_file() && config.is_file() {
        Some((dll, config))
    } else {
        None
    }
}

/// Locate the bridge DLL and runtime config.
///
/// Search order:
/// 1. `AL_BRIDGE_DIR` environment variable (explicit operator override)
/// 2. Next to the current executable (deployed)
/// 3. `OUT_DIR` from build.rs (development build artifact)
pub fn find_bridge_dll() -> Result<(PathBuf, PathBuf), SemanticError> {
    if let Ok(dir) = std::env::var("AL_BRIDGE_DIR") {
        let bridge_dir = PathBuf::from(&dir);
        let dll = bridge_dir.join("AlBridge.dll");
        let config = bridge_dir.join("AlBridge.runtimeconfig.json");
        if let Some(pair) = check_bridge_pair(dll, config) {
            debug!(path = %pair.0.display(), "Found bridge DLL from AL_BRIDGE_DIR");
            return Ok(pair);
        }
        return Err(SemanticError::HostInit(format!(
            "AL_BRIDGE_DIR '{}' does not contain AlBridge.dll and AlBridge.runtimeconfig.json",
            bridge_dir.display()
        )));
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let dll = exe_dir.join("bridge").join("AlBridge.dll");
            let config = exe_dir.join("bridge").join("AlBridge.runtimeconfig.json");
            if let Some(pair) = check_bridge_pair(dll, config) {
                debug!(path = %pair.0.display(), "Found bridge DLL next to executable");
                return Ok(pair);
            }
            // Also check flat layout
            let dll = exe_dir.join("AlBridge.dll");
            let config = exe_dir.join("AlBridge.runtimeconfig.json");
            if let Some(pair) = check_bridge_pair(dll, config) {
                debug!(path = %pair.0.display(), "Found bridge DLL next to executable (flat)");
                return Ok(pair);
            }
        }
    }

    if let Some(out_dir) = option_env!("OUT_DIR") {
        let bridge_dir = PathBuf::from(out_dir).join("bridge");
        let dll = bridge_dir.join("AlBridge.dll");
        let config = bridge_dir.join("AlBridge.runtimeconfig.json");
        if let Some(pair) = check_bridge_pair(dll, config) {
            debug!(path = %pair.0.display(), "Found bridge DLL from OUT_DIR");
            return Ok(pair);
        }
    }

    Err(SemanticError::HostInit(
        "Could not find AlBridge.dll. Set AL_BRIDGE_DIR or ensure bridge/ is compiled.".into(),
    ))
}

#[cfg(feature = "semantic")]
fn path_to_pdcstring(path: &Path) -> Result<netcorehost::pdcstring::PdCString, SemanticError> {
    netcorehost::pdcstring::PdCString::from_os_str(path.as_os_str())
        .map_err(|e| SemanticError::HostInit(format!("Invalid path for .NET: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn test_find_bridge_returns_error_when_not_found() {
        let prev = std::env::var_os("AL_BRIDGE_DIR");
        std::env::remove_var("AL_BRIDGE_DIR");
        let result = find_bridge_dll();
        if let Some(value) = prev {
            std::env::set_var("AL_BRIDGE_DIR", value);
        }
        match result {
            Ok((dll, config)) => {
                assert!(dll.is_file());
                assert!(config.is_file());
            }
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("AlBridge")
                        || msg.contains("bridge")
                        || msg.contains("dotnet")
                        || msg.contains("Not initialized"),
                    "unexpected bridge error: {msg}"
                );
            }
        }
    }

    #[test]
    #[cfg(not(feature = "semantic"))]
    fn test_stub_new_returns_not_initialized_without_semantic_feature() {
        use std::path::Path;
        let dummy = Path::new("/nonexistent");
        let result = DotNetHost::new(dummy, dummy, dummy);
        assert!(
            result.is_err(),
            "stub DotNetHost::new must always return Err"
        );
    }

    #[cfg(feature = "semantic")]
    #[test]
    #[serial]
    fn test_init_failure_surfaces_managed_exception_detail() {
        let (bridge_dll, runtime_config) = find_bridge_dll().expect("test bridge should be built");
        let dir = tempfile::tempdir().unwrap();
        let invalid = dir.path().join("Microsoft.Dynamics.Nav.CodeAnalysis.dll");
        std::fs::write(&invalid, b"not a managed assembly").unwrap();

        let error = match DotNetHost::new(&bridge_dll, &runtime_config, &invalid) {
            Ok(_) => panic!("invalid CodeAnalysis assembly must not initialize"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("Bridge Init failed"), "{error}");
        assert!(
            error.contains("BadImageFormat")
                || error.contains("image")
                || error.contains("assembly"),
            "managed exception detail should be preserved: {error}"
        );
    }

    #[test]
    fn test_check_bridge_pair_both_present() {
        let dir = tempfile::tempdir().unwrap();
        let dll = dir.path().join("AlBridge.dll");
        let config = dir.path().join("AlBridge.runtimeconfig.json");
        std::fs::write(&dll, b"fake dll").unwrap();
        std::fs::write(&config, b"{}").unwrap();

        let result = check_bridge_pair(dll.clone(), config.clone());
        assert_eq!(result, Some((dll, config)));
    }

    #[test]
    fn test_check_bridge_pair_config_missing_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let dll = dir.path().join("AlBridge.dll");
        let config = dir.path().join("AlBridge.runtimeconfig.json"); // not created
        std::fs::write(&dll, b"fake dll").unwrap();

        assert_eq!(check_bridge_pair(dll, config), None);
    }

    #[test]
    fn test_check_bridge_pair_dll_missing_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let dll = dir.path().join("AlBridge.dll"); // not created
        let config = dir.path().join("AlBridge.runtimeconfig.json");
        std::fs::write(&config, b"{}").unwrap();

        assert_eq!(check_bridge_pair(dll, config), None);
    }

    #[test]
    fn test_check_bridge_pair_neither_present_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let dll = dir.path().join("AlBridge.dll");
        let config = dir.path().join("AlBridge.runtimeconfig.json");

        assert_eq!(check_bridge_pair(dll, config), None);
    }

    /// A directory at the DLL path is not a regular file -> None. `is_file()`
    /// must reject directories; `exists()` alone would wrongly accept them.
    #[test]
    fn test_check_bridge_pair_directory_is_not_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let dll = dir.path().join("AlBridge.dll");
        let config = dir.path().join("AlBridge.runtimeconfig.json");
        std::fs::create_dir(&dll).unwrap(); // a dir, not a file
        std::fs::write(&config, b"{}").unwrap();

        assert_eq!(check_bridge_pair(dll, config), None);
    }

    /// Serialized because it mutates a process-global env var that other tests also read.
    ///
    /// The explicit environment override has highest precedence.
    #[test]
    #[serial]
    fn test_find_bridge_dll_uses_al_bridge_dir() {
        let prev = std::env::var("AL_BRIDGE_DIR").ok();

        let dir = tempfile::tempdir().unwrap();
        let dll = dir.path().join("AlBridge.dll");
        let config = dir.path().join("AlBridge.runtimeconfig.json");
        std::fs::write(&dll, b"fake dll").unwrap();
        std::fs::write(&config, b"{}").unwrap();

        std::env::set_var("AL_BRIDGE_DIR", dir.path());
        let result = find_bridge_dll();

        // restore env before asserting
        match prev {
            Some(v) => std::env::set_var("AL_BRIDGE_DIR", v),
            None => std::env::remove_var("AL_BRIDGE_DIR"),
        }

        let (got_dll, got_config) = result.expect("a valid bridge pair should resolve");
        assert_eq!(got_dll, dll);
        assert_eq!(got_config, config);
    }

    /// `AL_BRIDGE_DIR` set to a directory that is missing the config file must
    /// not resolve to the half-populated directory.
    #[test]
    #[serial]
    fn test_find_bridge_dll_al_bridge_dir_incomplete_does_not_resolve() {
        let prev = std::env::var("AL_BRIDGE_DIR").ok();

        let dir = tempfile::tempdir().unwrap();
        let dll = dir.path().join("AlBridge.dll");
        std::fs::write(&dll, b"fake dll").unwrap();
        // deliberately do NOT create the runtimeconfig.json

        std::env::set_var("AL_BRIDGE_DIR", dir.path());
        let result = find_bridge_dll();

        match prev {
            Some(v) => std::env::set_var("AL_BRIDGE_DIR", v),
            None => std::env::remove_var("AL_BRIDGE_DIR"),
        }

        assert!(
            result.is_err(),
            "an invalid explicit override must fail closed"
        );
    }
}
