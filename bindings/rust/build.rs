//! Builds the generated parser and external scanner.

fn main() {
    let src_dir = std::path::Path::new("src");

    let mut build = cc::Build::new();
    build.include(src_dir);
    build.file(src_dir.join("parser.c"));
    println!("cargo:rerun-if-changed=src/parser.c");

    let scanner = src_dir.join("scanner.c");
    if !scanner.is_file() {
        panic!(
            "required generated scanner is missing: {}",
            scanner.display()
        );
    }
    build.file(&scanner);
    println!("cargo:rerun-if-changed=src/scanner.c");
    println!("cargo:rerun-if-changed=src/keywords.c");

    build.warnings(false).flag_if_supported("-w");

    build.compile("tree-sitter-al");
}
