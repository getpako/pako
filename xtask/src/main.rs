//! Repository-local development automation.
//!
//! The xtask is intentionally a thin orchestration layer. It invokes the public
//! `pako-build` and `pako` binaries instead of duplicating package, OCI, or TUF
//! behavior inside developer tooling.

mod context;
mod dev;
mod process;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::context::Context;

#[derive(Debug, Parser)]
#[command(
    name = "xtask",
    about = "Run Pako repository development tasks",
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Manage the isolated local OCI, TUF, and Pako client environment.
    Dev {
        #[command(subcommand)]
        command: DevCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum DevCommand {
    /// Initialize local state when needed and start OCI and TUF services.
    Up,

    /// Stop local services without deleting their data.
    Down,

    /// Delete all local state and start a fresh environment.
    Reset,

    /// Build and publish a recipe to the local OCI and TUF repositories.
    Publish {
        /// Path to the recipe.toml file.
        recipe: PathBuf,

        /// Target to build; defaults to the current Linux host architecture.
        #[arg(long, value_name = "TARGET")]
        target: Option<String>,
    },

    /// Run the Pako client with isolated HOME and XDG directories.
    Pako {
        /// Arguments passed directly to the Pako CLI.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<String>,
    },

    /// Run build, publish, install, verify, launcher, and status checks.
    Smoke,
}

fn main() -> Result<()> {
    let context = Context::discover()?;

    match Cli::parse().command {
        Command::Dev { command } => dev::run(&context, command),
    }
}
