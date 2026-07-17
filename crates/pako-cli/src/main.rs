mod cli;
mod commands;
mod output;
mod repository;

use std::path::PathBuf;

use clap::Parser;
use pako_core::layout::Layout;

use crate::cli::Cli;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let operation_name = cli.operation_log_name();
    let log_directory =
        Layout::discover().map_or_else(|_| PathBuf::from("logs"), |layout| layout.state.join("logs"));
    let log = match pako_log::init(&log_directory, &operation_name, cli.verbose) {
        Ok(log) => log,
        Err(error) => {
            eprintln!("error: failed to initialize logging: {error:#}");
            std::process::exit(1);
        }
    };

    if let Err(error) = commands::run(cli).await {
        log::error!("{error:#}");
        eprintln!("details: {}", log.path().display());
        std::process::exit(1);
    }
}
