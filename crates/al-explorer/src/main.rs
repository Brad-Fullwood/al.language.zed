//! Thin binary entry point. All of al-explorer's logic lives in the
//! `al_explorer` library crate (see `src/lib.rs`); `main` just dispatches
//! into [`al_explorer::run`], which selects the TUI or the CLI based on
//! whether any subcommand arguments were given.

fn main() -> std::process::ExitCode {
    al_explorer::run()
}
