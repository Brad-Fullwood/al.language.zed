//! Document store — open file management with rope-based text.

use dashmap::DashMap;
use ropey::Rope;
use tower_lsp::lsp_types::{TextDocumentContentChangeEvent, Url};

/// Store for open documents.
pub struct DocumentStore {
    docs: DashMap<Url, Document>,
}

struct Document {
    text: Rope,
    version: i32,
}

impl DocumentStore {
    pub fn new() -> Self {
        Self {
            docs: DashMap::new(),
        }
    }

    pub fn open(&self, uri: Url, text: String) {
        self.docs.insert(
            uri,
            Document {
                text: Rope::from_str(&text),
                version: 0,
            },
        );
    }

    pub fn close(&self, uri: &Url) {
        self.docs.remove(uri);
    }

    pub fn get_text(&self, uri: &Url) -> Option<String> {
        self.docs.get(uri).map(|d| d.text.to_string())
    }

    pub fn apply_changes(&self, uri: &Url, changes: &[TextDocumentContentChangeEvent]) {
        if let Some(mut doc) = self.docs.get_mut(uri) {
            for change in changes {
                if let Some(range) = change.range {
                    let start = position_to_offset(&doc.text, range.start);
                    let end = position_to_offset(&doc.text, range.end);
                    if let (Some(start), Some(end)) = (start, end) {
                        doc.text.remove(start..end);
                        doc.text.insert(start, &change.text);
                    }
                } else {
                    doc.text = Rope::from_str(&change.text);
                }
            }
            doc.version += 1;
        }
    }
}

fn position_to_offset(
    rope: &Rope,
    pos: tower_lsp::lsp_types::Position,
) -> Option<usize> {
    let line = pos.line as usize;
    if line >= rope.len_lines() {
        return None;
    }
    let line_start = rope.line_to_char(line);
    Some(line_start + pos.character as usize)
}
