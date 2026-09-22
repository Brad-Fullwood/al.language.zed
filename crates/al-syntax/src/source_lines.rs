//! Line lookup over a source buffer: a scanning helper for one-off reads and a
//! precomputed table for repeated ones.

/// Return the text of a single source line by zero-based `row` index.
///
/// Returns an empty string if `row` is out of range or the bytes are not valid UTF-8.
/// Uses `splitn` to avoid scanning past the requested line. Callers that read
/// many lines of the same file should build a [`SourceLines`] instead.
pub fn get_source_line(source: &[u8], row: usize) -> &str {
    source
        .splitn(row + 2, |&b| b == b'\n')
        .nth(row)
        .and_then(|b| std::str::from_utf8(b).ok())
        .unwrap_or("")
}

/// A source buffer with its line starts precomputed, so looking a line up by
/// row costs a table index instead of a scan from byte 0.
///
/// [`get_source_line`] walks the buffer on every call. A query that asks for
/// one line per result — inlay hints ask for two per hint, and the client
/// re-requests them on every scroll — therefore costs
/// O(results x file_length). Building this once per request makes it linear.
pub struct SourceLines<'a> {
    source: &'a [u8],
    starts: Vec<usize>,
}

impl<'a> SourceLines<'a> {
    #[must_use]
    pub fn new(source: &'a [u8]) -> Self {
        let starts = std::iter::once(0)
            .chain(
                source
                    .iter()
                    .enumerate()
                    .filter(|(_, &byte)| byte == b'\n')
                    .map(|(index, _)| index + 1),
            )
            .collect();
        Self { source, starts }
    }

    /// Byte offset of the first byte of line `row`.
    #[must_use]
    pub fn line_start(&self, row: usize) -> Option<usize> {
        self.starts.get(row).copied()
    }

    /// Bytes of line `row` including its terminator, or an empty slice when
    /// `row` is past the end.
    #[must_use]
    pub fn line_bytes(&self, row: usize) -> &'a [u8] {
        let Some(&start) = self.starts.get(row) else {
            return &[];
        };
        let end = self
            .starts
            .get(row + 1)
            .copied()
            .unwrap_or(self.source.len());
        self.source.get(start..end).unwrap_or(&[])
    }

    /// Text of line `row` without its terminator.
    ///
    /// Empty for a row past the end of the buffer or for bytes that are not
    /// valid UTF-8, matching [`get_source_line`].
    #[must_use]
    pub fn line(&self, row: usize) -> &'a str {
        let Some(&start) = self.starts.get(row) else {
            return "";
        };
        let end = self
            .starts
            .get(row + 1)
            .map_or(self.source.len(), |next| next - 1);
        std::str::from_utf8(&self.source[start..end.max(start)]).unwrap_or("")
    }

    /// Number of lines, counting a trailing newline as ending the last line.
    #[must_use]
    pub fn len(&self) -> usize {
        self.starts.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.source.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{get_source_line, SourceLines};

    const BUFFERS: [&[u8]; 6] = [
        b"line0\nline1\nline2",
        b"line0\nline1\n",
        b"only-one-line",
        b"",
        b"crlf\r\nsecond\r\n",
        b"ok\n\xff\xfe\ntail",
    ];

    /// The table must answer exactly what `get_source_line` answers, for every
    /// row and for the edges: out of range, invalid UTF-8, CRLF, a trailing
    /// newline and an empty buffer.
    #[test]
    fn line_matches_the_scanning_lookup() {
        for source in BUFFERS {
            let lines = SourceLines::new(source);
            for row in 0..10 {
                assert_eq!(
                    lines.line(row),
                    get_source_line(source, row),
                    "row {row} of {source:?}"
                );
            }
        }
    }

    #[test]
    fn line_bytes_keeps_the_terminator_and_line_start_addresses_it() {
        for source in BUFFERS {
            let lines = SourceLines::new(source);
            let mut expected_start = 0;
            for row in 0..10 {
                let bytes = lines.line_bytes(row);
                if row < lines.len() {
                    assert_eq!(lines.line_start(row), Some(expected_start), "{source:?}");
                    assert_eq!(&source[expected_start..expected_start + bytes.len()], bytes);
                    expected_start += bytes.len();
                } else {
                    assert_eq!(lines.line_start(row), None, "{source:?}");
                    assert!(bytes.is_empty(), "{source:?}");
                }
            }
            assert_eq!(expected_start, source.len(), "{source:?}");
        }
    }

    #[test]
    fn reports_its_line_count() {
        assert_eq!(SourceLines::new(b"a\nb\nc").len(), 3);
        assert_eq!(SourceLines::new(b"a\nb\n").len(), 3);
        assert!(SourceLines::new(b"").is_empty());
    }
}
