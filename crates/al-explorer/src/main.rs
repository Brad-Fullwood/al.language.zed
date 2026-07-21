//! Thin binary entry point. All of al-explorer's logic lives in the
//! `al_explorer` library crate (see `src/lib.rs`); `main` just dispatches
//! into [`al_explorer::run`], which selects the TUI or the CLI based on
//! whether any subcommand arguments were given.

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    al_explorer::run()
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    // Windows executables reserve a much smaller main-thread stack than the
    // other supported platforms. Building clap's large command tree can
    // exhaust that reserve before even a lightweight command such as
    // `version` is dispatched. Run the application on an explicitly sized
    // stack while preserving its exit code and panic behaviour.
    let handle = match std::thread::Builder::new()
        .name("al-explorer-main".to_string())
        .stack_size(8 * 1024 * 1024)
        .spawn(al_explorer::run)
    {
        Ok(handle) => handle,
        Err(error) => {
            eprintln!("failed to start al-explorer: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };

    match handle.join() {
        Ok(exit_code) => exit_code,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}
