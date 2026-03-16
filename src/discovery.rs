use std::collections::HashMap;

/// Find the proxy binary path.
///
/// Returns `Some(path)` only if the binary actually exists on disk.
/// Uses `worktree.shell_env()` as the primary source since `std::env` doesn't work in WASM.
/// Returns `None` if HOME is missing or the binary is not present — callers must then
/// fall back to the user-configured path from LSP settings.
pub fn find_proxy_path(env_map: &HashMap<String, String>) -> Option<String> {
    let home = env_map.get("HOME")?;
    let platform = crate::platform::detect_platform(env_map);
    let extensions_base = platform.extensions_base(home);
    let proxy_path = format!(
        "{}/al.language.zed/bin/{}/{}",
        extensions_base,
        platform.bin_dir(),
        platform.binary_name()
    );
    // Only return the path if the binary is actually present.
    // std::fs::metadata works in WASM (it maps to WASI fs calls).
    if std::fs::metadata(&proxy_path).is_ok() {
        Some(proxy_path)
    } else {
        None
    }
}
