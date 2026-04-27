fn main() {
    let src_dir = std::path::Path::new("../../tree-sitter-al/src");

    let mut build = cc::Build::new();
    build.include(src_dir).file(src_dir.join("parser.c"));

    // Add scanner if it exists
    let scanner = src_dir.join("scanner.c");
    if scanner.exists() {
        build.file(scanner);
    }

    // Silence warnings from tree-sitter's generated parser.c — we don't control its output.
    build.warnings(false).flag_if_supported("-w");

    build.compile("tree-sitter-al");
}
