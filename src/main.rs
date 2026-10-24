#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use clap::Parser;
use mho::cli::Cli;
use mho::cli::commands::handle;
use mho::error::Result;
use mho::log::{LogConfig, init_logger};
use mho::{log_error, log_info};

/// Apply the console policy for this launch.
///
/// Console behavior is resolved at runtime by `consolex`:
///
/// * **Inside an existing terminal** — behaves as a normal CLI program.
/// * **`--show`** — always opens a new console window, even from a terminal.
/// * **`--hide`** — detaches the console and suppresses all output.
/// * **No flags, no terminal** (double-clicked / third-party) — opens a window.
///
/// Scheduled runs always force [`consolex::Mode::Hide`] so the task does not
/// flash a window every 10 minutes.
///
/// Returns `true` when a new console window was created.
#[cfg(windows)]
fn init_console() -> bool {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mode = if args.iter().any(|arg| arg == "--schedule") {
        consolex::Mode::Hide
    } else {
        // `--show` / `--hide`, otherwise `Mode::Auto`.
        args.iter().collect::<consolex::Mode>()
    };

    match consolex::init(mode) {
        Ok(created) => created,
        Err(e) => {
            eprintln!("Failed to initialize console: {}", e);
            false
        }
    }
}

/// On non-Windows platforms the console is managed by the shell.
#[cfg(not(windows))]
fn init_console() -> bool {
    false
}

/// Keep a newly created window open long enough to read the output.
#[cfg(windows)]
fn wait_if_created(created: bool) {
    if created {
        let _ = consolex::wait_key();
    }
}

#[cfg(not(windows))]
fn wait_if_created(_created: bool) {}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    // Resolve the console before anything writes to stdout or stderr.
    let created_window = init_console();

    // Initialize logging system
    if let Err(e) = init_logging() {
        eprintln!("Failed to initialize logging: {}", e);
        // Continue without logging rather than failing
    }

    log_info!("Mho application starting");

    // Run the application and handle errors
    if let Err(e) = run().await {
        log_error!("Application error: {}", e);
        eprintln!("Error: {}", e);
        wait_if_created(created_window);
        std::process::exit(1);
    }

    log_info!("Mho application exiting");
    wait_if_created(created_window);
}

/// Initialize the logging system
fn init_logging() -> Result<()> {
    let config = LogConfig::default();

    init_logger(config)?;
    Ok(())
}

/// Main application logic
async fn run() -> Result<()> {
    let cli = Cli::parse();

    log_info!("Parsed CLI arguments");
    handle(cli.command).await?;
    Ok(())
}
