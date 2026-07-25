//! The `nexusnet` command-line interface.
//!
//! In this foundation phase the CLI is a thin, dependency-light front end over
//! [`nexusnet_core`]. It can report build information and resolve/validate a
//! configuration from the environment. Long-running serving commands arrive
//! once the transport layer lands.
//!
//! ## Usage
//!
//! ```text
//! nexusnet <command>
//!
//! Commands:
//!   version    Print the engine version and exit.
//!   info       Resolve configuration from NEXUSNET_* env vars and print it.
//!   help       Print this help text.
//! ```

use std::process::ExitCode;

use nexusnet_core::{version_string, Engine, EngineConfig};

/// The command requested on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Version,
    Info,
    Help,
}

impl Command {
    /// Parses the first positional argument into a [`Command`].
    ///
    /// Absent or unrecognized commands map to [`Command::Help`] so the tool is
    /// forgiving and self-documenting.
    fn parse(arg: Option<&str>) -> Self {
        match arg {
            Some("version" | "--version" | "-V") => Self::Version,
            Some("info") => Self::Info,
            _ => Self::Help,
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let command = Command::parse(args.get(1).map(String::as_str));

    match command {
        Command::Version => {
            println!("{}", version_string());
            ExitCode::SUCCESS
        }
        Command::Info => run_info(),
        Command::Help => {
            print_help();
            ExitCode::SUCCESS
        }
    }
}

/// Resolves configuration from the environment, validates it by building an
/// engine, and prints a summary. Returns a failure exit code on invalid input.
fn run_info() -> ExitCode {
    let config = match EngineConfig::default().with_env_overrides() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };

    match Engine::with_config(config) {
        Ok(engine) => {
            let config = engine.config();
            println!("{}", version_string());
            println!("name            : {}", config.name);
            println!("log level       : {}", config.log_level);
            println!("log format      : {}", config.log_format);
            println!(
                "worker threads  : {}",
                config
                    .worker_threads
                    .map_or_else(|| "auto".to_owned(), |n| n.to_string())
            );
            println!("shutdown timeout: {:?}", config.shutdown_timeout);
            println!("metrics enabled : {}", config.metrics_enabled);
            println!("state           : {}", engine.state());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Prints usage information to standard output.
fn print_help() {
    println!("{}", version_string());
    println!();
    println!("Usage: nexusnet <command>");
    println!();
    println!("Commands:");
    println!("  version    Print the engine version and exit.");
    println!("  info       Resolve configuration from NEXUSNET_* env vars and print it.");
    println!("  help       Print this help text.");
}
