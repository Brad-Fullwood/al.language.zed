//! Reads of files that a project or an index takes from disk.
//!
//! A path that is not a regular file, such as a FIFO, is refused without a
//! read that waits for a writer that never comes.

use std::path::Path;

// Windows tools write a UTF-8 BOM, which serde_json rejects.
pub fn strip_utf8_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

/// The text `reader` holds without a leading BOM, or `None` when it holds more
/// than `cap` bytes. Reads at most `cap + 1` bytes, so a source that never ends
/// stops there.
pub fn read_text_up_to(reader: impl std::io::Read, cap: u64) -> std::io::Result<Option<String>> {
    use std::io::Read;

    let mut text = String::new();
    reader.take(cap + 1).read_to_string(&mut text)?;
    Ok((text.len() as u64 <= cap).then(|| strip_utf8_bom(&text).to_owned()))
}

/// The text of the regular file at `path`, read up to `cap` bytes.
///
/// `Ok(None)` when nothing exists at `path`. A path that is not a regular
/// file, such as a link to `/dev/zero` or to a FIFO, is refused without a
/// read that waits or never ends. So is a file larger than `cap`.
pub fn read_regular_text_file(path: &Path, cap: u64) -> Result<Option<String>, String> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot inspect file: {error}")),
    };
    let too_large = || format!("larger than {cap} bytes");
    if !metadata.is_file() {
        return Err("not a regular file".to_string());
    }
    if metadata.len() > cap {
        return Err(too_large());
    }
    let file = open_regular_file(path).ok_or("not a regular file")?;
    read_text_up_to(file, cap)
        .map_err(|error| format!("cannot read file: {error}"))?
        .ok_or_else(too_large)
        .map(Some)
}

/// `path` opened for reading, when it is a regular file once open.
///
/// On Unix the open does not wait. A file swapped for a FIFO after it was
/// checked opens at once and is refused here, where a plain open blocked
/// until something wrote to the FIFO.
pub fn open_regular_file(path: &Path) -> Option<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    file.metadata()
        .ok()
        .filter(std::fs::Metadata::is_file)
        .map(|_| file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_text_up_to_stops_one_byte_past_the_cap() {
        use std::io::Read;

        let mut endless = std::io::repeat(b'a').take(1 << 20);
        assert_eq!(read_text_up_to(&mut endless, 1024).unwrap(), None);
        assert_eq!(endless.limit(), (1 << 20) - 1025, "bytes taken from it");

        let at_the_cap = read_text_up_to(std::io::repeat(b'a').take(1024), 1024).unwrap();
        assert_eq!(at_the_cap.map(|text| text.len()), Some(1024));
    }

    #[test]
    fn read_text_up_to_drops_a_utf8_bom() {
        assert_eq!(
            read_text_up_to(&b"\xEF\xBB\xBF{}"[..], 64).unwrap(),
            Some("{}".to_string())
        );
    }
}
