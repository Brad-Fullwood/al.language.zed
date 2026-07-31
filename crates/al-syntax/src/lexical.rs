//! Allocation-free lexical spans for the small text-based syntax helpers.
//!
//! This is intentionally not a replacement for tree-sitter. It is the shared
//! source of truth for the formatter/sorter helpers that only need to
//! distinguish AL code from quoted text and comments on one physical line.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpanKind {
    Code,
    String,
    LineComment,
    BlockComment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Span<'a> {
    pub(crate) kind: SpanKind,
    pub(crate) start: usize,
    pub(crate) text: &'a str,
}

/// Iterate over code, quoted text, and comment spans in one physical AL line.
///
/// `in_block_comment` carries an unterminated `/* ...` from the preceding
/// line. Consume the iterator before reading [`Self::ends_in_block_comment`].
pub(crate) struct LineScanner<'a> {
    line: &'a str,
    offset: usize,
    in_block_comment: bool,
}

impl<'a> LineScanner<'a> {
    pub(crate) fn new(line: &'a str, in_block_comment: bool) -> Self {
        Self {
            line,
            offset: 0,
            in_block_comment,
        }
    }

    pub(crate) fn ends_in_block_comment(&self) -> bool {
        self.in_block_comment
    }

    fn block_comment(&mut self, start: usize, content_start: usize) -> Span<'a> {
        let bytes = self.line.as_bytes();
        let mut i = content_start;
        while i + 1 < bytes.len() {
            if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                self.offset = i + 2;
                self.in_block_comment = false;
                return Span {
                    kind: SpanKind::BlockComment,
                    start,
                    text: &self.line[start..self.offset],
                };
            }
            i += 1;
        }

        self.offset = bytes.len();
        self.in_block_comment = true;
        Span {
            kind: SpanKind::BlockComment,
            start,
            text: &self.line[start..],
        }
    }
}

impl<'a> Iterator for LineScanner<'a> {
    type Item = Span<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let bytes = self.line.as_bytes();
        if self.offset >= bytes.len() {
            return None;
        }

        let start = self.offset;
        if self.in_block_comment {
            return Some(self.block_comment(start, start));
        }

        match bytes[start] {
            b'\'' | b'"' => {
                let quote = bytes[start];
                let mut i = start + 1;
                while i < bytes.len() {
                    if bytes[i] == quote {
                        // AL represents a quote inside either quoted form by
                        // doubling it. The pair is content, not a terminator.
                        if i + 1 < bytes.len() && bytes[i + 1] == quote {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                self.offset = i;
                Some(Span {
                    kind: SpanKind::String,
                    start,
                    text: &self.line[start..i],
                })
            }
            b'/' if start + 1 < bytes.len() && bytes[start + 1] == b'/' => {
                self.offset = bytes.len();
                Some(Span {
                    kind: SpanKind::LineComment,
                    start,
                    text: &self.line[start..],
                })
            }
            b'/' if start + 1 < bytes.len() && bytes[start + 1] == b'*' => {
                self.in_block_comment = true;
                Some(self.block_comment(start, start + 2))
            }
            _ => {
                let mut i = start + 1;
                while i < bytes.len() {
                    if matches!(bytes[i], b'\'' | b'"')
                        || (bytes[i] == b'/'
                            && i + 1 < bytes.len()
                            && matches!(bytes[i + 1], b'/' | b'*'))
                    {
                        break;
                    }
                    i += 1;
                }
                self.offset = i;
                Some(Span {
                    kind: SpanKind::Code,
                    start,
                    text: &self.line[start..i],
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LineScanner, SpanKind};

    #[test]
    fn yields_code_string_and_comment_spans_without_losing_utf8() {
        let mut scanner = LineScanner::new(
            "if Name = 'Grüße ''x''' then /* begin */ Foo(); // end",
            false,
        );
        let spans: Vec<_> = scanner.by_ref().collect();

        assert_eq!(
            spans
                .iter()
                .map(|span| (span.kind, span.text))
                .collect::<Vec<_>>(),
            vec![
                (SpanKind::Code, "if Name = "),
                (SpanKind::String, "'Grüße ''x'''"),
                (SpanKind::Code, " then "),
                (SpanKind::BlockComment, "/* begin */"),
                (SpanKind::Code, " Foo(); "),
                (SpanKind::LineComment, "// end"),
            ]
        );
        assert!(!scanner.ends_in_block_comment());
    }

    #[test]
    fn carries_block_comment_state_between_lines() {
        let mut opening = LineScanner::new("code /* open", false);
        let opening_spans: Vec<_> = opening.by_ref().collect();
        assert_eq!(opening_spans.last().unwrap().kind, SpanKind::BlockComment);
        assert!(opening.ends_in_block_comment());

        let mut closing = LineScanner::new("still comment */ end;", true);
        let closing_spans: Vec<_> = closing.by_ref().collect();
        assert_eq!(
            closing_spans
                .iter()
                .map(|span| (span.kind, span.text))
                .collect::<Vec<_>>(),
            vec![
                (SpanKind::BlockComment, "still comment */"),
                (SpanKind::Code, " end;"),
            ]
        );
        assert!(!closing.ends_in_block_comment());
    }
}
