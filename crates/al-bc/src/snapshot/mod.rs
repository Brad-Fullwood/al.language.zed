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
}

/// Largest CPU profile read out of a finished snapshot zip.
const MAX_CPU_PROFILE_BYTES: u64 = 500 * 1024 * 1024;

/// Save the bytes `finish` returned as `<dir>/<ctx>.zip`. When `sampling` is
/// true (the response ETag was `"Sampling"`) the zip holds `<ctx>.alcpuprofile`,
/// which is also written beside it. Both writes refuse a symbolic link or other
/// non regular file at the destination. Standard route checked: the `zip` crate
/// already in this crate reads the entry, and `output_file::write_no_follow`
/// does the guarded write, so nothing is reimplemented here.
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
    let profile = if sampling {
        let entry = format!("{ctx}.alcpuprofile");
        let zip_bytes = bytes.clone();
        let data = tokio::task::spawn_blocking(move || read_zip_entry(&zip_bytes, &entry))
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))??;
        Some(data)
    } else {
        None
    };
    crate::output_file::write_no_follow(&zip_path, bytes).await?;
    let cpu_profile = match profile {
        Some(data) => {
            let path = dir.join(format!("{ctx}.alcpuprofile"));
            crate::output_file::write_no_follow(&path, data).await?;
            Some(path)
        }
        None => None,
    };
    Ok(SavedSnapshot {
        zip: zip_path,
        cpu_profile,
    })
}

fn read_zip_entry(zip_bytes: &[u8], name: &str) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let invalid =
        |e: zip::result::ZipError| std::io::Error::new(std::io::ErrorKind::InvalidData, e);
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).map_err(invalid)?;
    let entry = archive.by_name(name).map_err(invalid)?;
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
    Ok(out)
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

    use super::*;
}
