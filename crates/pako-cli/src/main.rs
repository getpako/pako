mod cli;
mod commands;
mod output;
mod repository;

use clap::Parser;

use crate::cli::Cli;

#[tokio::main]
async fn main() {
    if let Err(error) = commands::run(Cli::parse()).await {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
