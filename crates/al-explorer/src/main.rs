//! Thin binary entry point. All of al-explorer's logic lives in the
//! `al_explorer` library crate (see `src/lib.rs`); `main` just dispatches
//! into [`al_explorer::run`], which selects the TUI or the CLI based on
//! whether any subcommand arguments were given.

fn run_application() -> std::process::ExitCode {
    // Rust ignores SIGPIPE by default, which turns a normal Unix pipeline such
    // as `al-explorer ... | head` into a panic when `head` closes the pipe.
    // CLI tools conventionally restore the default disposition so a closed
    // downstream reader terminates the producer quietly.
    #[cfg(unix)]
    unsafe {
        // SAFETY: this runs once, on the initial main thread, before the
        // application creates worker threads or installs signal handlers.
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    al_explorer::run()
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    run_application()
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
        .spawn(run_application)
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
