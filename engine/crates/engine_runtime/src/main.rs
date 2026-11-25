//! Entry point for the `engine_runtime` executable.

use std::process::ExitCode;

use bevy::app::AppExit;
use clap::Parser;
use engine_runtime::{run_from_cli, RuntimeCli};

fn main() -> ExitCode {
    let cli = RuntimeCli::parse();
    match run_from_cli(cli) {
        Ok(AppExit::Success) => ExitCode::SUCCESS,
        Ok(AppExit::Error(code)) => ExitCode::from(code.get()),
        Err(err) => {
            eprintln!("engine_runtime failed: {err:#}");
            ExitCode::FAILURE
        }
    }
}
