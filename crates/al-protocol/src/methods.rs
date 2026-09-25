//! Daemon method names both ends of the socket need to agree on.

/// The daemon methods that answer from one file and never write it.
///
/// The daemon refuses a path outside the project it has loaded, because the
/// same dispatchers are published over MCP, where the caller may be an agent
/// and the path may be anything it asks for. The person running the CLI can
/// already read their own files, so for these methods the client reads the
/// file and sends its `text`, and the daemon answers without opening the path.
///
/// Methods that rewrite the file are absent on purpose: content a caller
/// supplies can be analysed, never written back over a path the daemon was not
/// allowed to name.
///
/// The list lives here because the daemon and `al-explorer` are two crates and
/// one contract. `al-lsp`'s dispatch table declares the same capability per
/// method and its tests pin the two together, so a new read-only method cannot
/// appear in one and not the other.
pub const TEXT_CAPABLE_METHODS: &[&str] = &[
    "codeActions",
    "completions",
    "definition",
    "documentSymbols",
    "foldingRanges",
    "hover",
    "implementations",
    "inlayHints",
    "lint",
    "metrics",
    "parse",
    "references",
    "rename",
    "semanticTokens",
    "signatureHelp",
];

#[cfg(test)]
mod tests {
    use super::TEXT_CAPABLE_METHODS;

    #[test]
    fn the_list_is_sorted_and_has_no_duplicate() {
        let mut sorted = TEXT_CAPABLE_METHODS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted, TEXT_CAPABLE_METHODS,
            "keep the list sorted so a diff shows what changed"
        );
    }
}
