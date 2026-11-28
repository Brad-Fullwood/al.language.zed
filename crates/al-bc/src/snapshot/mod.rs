//! Snapshot debugger support: the archive reader and the session store, plus
//! `save_finished_snapshot` for the zip a `finish` call returns. The REST calls
//! live in `al-dap` (`bc_debug::snapshot_debugger`).

pub mod archive;
pub mod state;

use std::path::PathBuf;

/// Paths written by [`save_finished_snapshot`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedSnapshot {
    /// `<dir>/<ctx>.zip`
    pub zip: PathBuf,
    /// `<dir>/<ctx>.alcpuprofile`, present when the zip held a CPU profile.
    pub cpu_profile: Option<PathBuf>,
    /// Why the CPU profile was not saved. The zip is saved either way.
    pub profile_error: Option<String>,
}

/// Largest CPU profile read out of a finished snapshot zip.
const MAX_CPU_PROFILE_BYTES: u64 = 500 * 1024 * 1024;

/// Save the bytes `finish` returned as `<dir>/<ctx>.zip`. The zip is written
/// first. When `sampling` is true (the response ETag was `"Sampling"`) the zip
/// holds `<ctx>.alcpuprofile`, which is also written beside it. A profile that
/// cannot be read or written is reported in `profile_error` and does not fail
/// the call. Both writes refuse a symbolic link or other non regular file at the
/// destination. Standard route checked: the `zip` crate already in this crate
/// reads the entry, and `output_file::write_no_follow` does the guarded write,
/// so nothing is reimplemented here.
pub async fn save_finished_snapshot(
    dir: &std::path::Path,
    ctx: &str,
    bytes: Vec<u8>,
    sampling: bool,
) -> std::io::Result<SavedSnapshot> {
    if ctx.is_empty() || !ctx.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("debugging context `{ctx}` is not a GUID"),
        ));
    }
    let zip_path = dir.join(format!("{ctx}.zip"));
    let profile_source = sampling.then(|| bytes.clone());
    crate::output_file::write_no_follow(&zip_path, bytes).await?;
    let (cpu_profile, profile_error) = match profile_source {
        None => (None, None),
        Some(zip_bytes) => match save_cpu_profile(dir, ctx, zip_bytes).await {
            Ok(path) => (path, None),
            Err(error) => (
                None,
                Some(format!("The CPU profile was not saved: {error}")),
            ),
        },
    };
    Ok(SavedSnapshot {
        zip: zip_path,
        cpu_profile,
        profile_error,
    })
}

/// Extract `<ctx>.alcpuprofile` from the zip and write it beside the zip.
/// `None` when the zip has no such entry.
async fn save_cpu_profile(
    dir: &std::path::Path,
    ctx: &str,
    zip_bytes: Vec<u8>,
) -> std::io::Result<Option<PathBuf>> {
    let entry = format!("{ctx}.alcpuprofile");
    let data = tokio::task::spawn_blocking(move || read_zip_entry(&zip_bytes, &entry))
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))??;
    match data {
        Some(data) => {
            let path = dir.join(format!("{ctx}.alcpuprofile"));
            crate::output_file::write_no_follow(&path, data).await?;
            Ok(Some(path))
        }
        None => Ok(None),
    }
}

/// `None` when the zip has no entry of that name (a sampling run that recorded
/// nothing), so the zip is still saved.
fn read_zip_entry(zip_bytes: &[u8], name: &str) -> std::io::Result<Option<Vec<u8>>> {
    use std::io::Read;
    let invalid =
        |e: zip::result::ZipError| std::io::Error::new(std::io::ErrorKind::InvalidData, e);
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).map_err(invalid)?;
    let entry = match archive.by_name(name) {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(invalid(e)),
    };
    let mut out = Vec::new();
    entry
        .take(MAX_CPU_PROFILE_BYTES + 1)
        .read_to_end(&mut out)?;
    if out.len() as u64 > MAX_CPU_PROFILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "CPU profile in the snapshot zip is larger than 500 MB",
        ));
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {

    fn zip_with(name: &str, data: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        w.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        w.write_all(data).unwrap();
        w.finish().unwrap().into_inner()
    }

    /// The zip a CDX Sandbox returned for a sampling profile with web service
    /// activity (2026-10-27). The server sent no ETag and the profile was
    /// `{"kind":1}`, an empty profile.
    #[tokio::test]
    async fn recorded_cdx_sampling_zip_yields_its_alcpuprofile() {
        let bytes = include_bytes!("../../tests/fixtures/cdx-sampling-profile-empty.zip").to_vec();
        let ctx = "468b5f81-7e1f-46ea-9827-9c5a5cef4728";
        let dir = tempfile::tempdir().unwrap();
        let saved = save_finished_snapshot(dir.path(), ctx, bytes, true)
            .await
            .unwrap();
        let profile = saved.cpu_profile.expect("entry extracted");
        assert_eq!(std::fs::read_to_string(profile).unwrap(), "{\"kind\":1}");
    }

    #[tokio::test]
    async fn save_finished_snapshot_writes_zip_and_extracts_profile() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = "c4fbb573-e325-443c-9887-8d0574e833ee";
        let bytes = zip_with(&format!("{ctx}.alcpuprofile"), b"{\"nodes\":[]}");
        let saved = save_finished_snapshot(dir.path(), ctx, bytes.clone(), true)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&saved.zip).unwrap(), bytes);
        let profile = saved.cpu_profile.unwrap();
        assert_eq!(profile, dir.path().join(format!("{ctx}.alcpuprofile")));
        assert_eq!(std::fs::read(profile).unwrap(), b"{\"nodes\":[]}");
    }

    #[tokio::test]
    async fn save_finished_snapshot_keeps_the_zip_when_the_profile_entry_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = "c4fbb573-e325-443c-9887-8d0574e833ee";
        let bytes = zip_with("other.txt", b"x");
        let saved = save_finished_snapshot(dir.path(), ctx, bytes.clone(), true)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&saved.zip).unwrap(), bytes);
        assert!(saved.cpu_profile.is_none());
    }

    #[tokio::test]
    async fn save_finished_snapshot_without_sampling_writes_only_the_zip() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = "ed75bfc5-a198-4fe6-ad81-54b95d34965a";
        let saved = save_finished_snapshot(dir.path(), ctx, zip_with("version", b"x"), false)
            .await
            .unwrap();
        assert!(saved.cpu_profile.is_none());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn save_finished_snapshot_rejects_a_context_with_path_separators() {
        let dir = tempfile::tempdir().unwrap();
        let err = save_finished_snapshot(dir.path(), "../evil", vec![1], false).await;
        assert!(err.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn save_finished_snapshot_refuses_a_symlink_at_the_destination() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        std::fs::write(&target, b"keep").unwrap();
        let ctx = "ed75bfc5-a198-4fe6-ad81-54b95d34965a";
        std::os::unix::fs::symlink(&target, dir.path().join(format!("{ctx}.zip"))).unwrap();
        let err = save_finished_snapshot(dir.path(), ctx, vec![1, 2, 3], false).await;
        assert!(err.is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
    }

    #[tokio::test]
    async fn corrupt_profile_entry_still_saves_zip() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = "c4fbb573-e325-443c-9887-8d0574e833ee";
        let bytes = b"not a zip".to_vec();
        let saved = save_finished_snapshot(dir.path(), ctx, bytes.clone(), true)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&saved.zip).unwrap(), bytes);
        assert!(saved.cpu_profile.is_none());
        let error = saved.profile_error.expect("profile error is reported");
        assert!(error.contains("The CPU profile was not saved"), "{error}");
    }

    use super::*;
}
