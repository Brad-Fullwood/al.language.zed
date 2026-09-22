//! Naming for the temporary file that an atomic write renames into place.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Temporary path beside `target`, hidden and unique within the process.
///
/// The name is `.{target file name}.{pid}.{sequence}.tmp`, so two writers of
/// the same destination (two threads here, or two al-lsp processes over one
/// project directory) never pick the same temporary file and a leftover one is
/// recognisable. `fallback` names the file when `target` has no usable file
/// name.
pub(crate) fn beside(target: &Path, fallback: &str) -> PathBuf {
    let filename = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(fallback);
    target.with_file_name(format!(".{filename}.{}.tmp", unique_token()))
}

/// `{pid}.{sequence}`, distinct on every call in this process and distinct
/// from any other process's tokens.
pub(crate) fn unique_token() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{}.{sequence}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::{beside, unique_token};
    use std::path::Path;

    #[test]
    fn successive_paths_for_one_target_differ() {
        let target = Path::new("/tmp/project/Table.al");
        let first = beside(target, "symbol.al");
        let second = beside(target, "symbol.al");
        assert_ne!(first, second);
        for path in [&first, &second] {
            assert_eq!(path.parent(), target.parent());
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .expect("file name");
            assert!(name.starts_with(".Table.al."), "{name}");
            assert!(name.ends_with(".tmp"), "{name}");
        }
    }

    #[test]
    fn a_target_without_a_file_name_uses_the_fallback() {
        let path = beside(Path::new(".."), "entry");
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("file name");
        assert!(name.starts_with(".entry."), "{name}");
    }

    #[test]
    fn tokens_carry_the_process_id_and_do_not_repeat() {
        let first = unique_token();
        let second = unique_token();
        assert_ne!(first, second);
        let pid = std::process::id().to_string();
        assert!(first.starts_with(&format!("{pid}.")), "{first}");
    }
}
