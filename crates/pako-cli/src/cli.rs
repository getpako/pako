use std::num::NonZeroUsize;

use clap::{ArgAction, Args, Parser, Subcommand};

const ROOT_LONG_ABOUT: &str = "\
Pako is a user-space package manager for Linux.

Packages are installed into user-owned XDG directories. Pako resolves releases
from a configured, trusted repository, downloads only the content which is not
already available locally, verifies the complete package tree, and atomically
activates the new version.

Normal package operations do not require root privileges. Installed versions
remain isolated from one another so that a previous version can be activated
with `pako rollback`.";

const ROOT_AFTER_HELP: &str = "\
Getting started:
  pako install vscodium
  pako list
  pako status vscodium
  pako upgrade vscodium --dry-run
  pako upgrade vscodium
  pako verify vscodium

Run `pako <COMMAND> --help` for detailed documentation and examples for a
specific command.";

const INSTALL_LONG_ABOUT: &str = "\
Install a package from the configured Pako repository.

Pako resolves the requested package, release channel, and host architecture
using signed repository metadata. It then downloads the package manifest, pack
index, and only those immutable packs which contain chunks missing from the
local object store.

The package is materialized into a staging directory and the complete tree is
verified before activation. Activation is atomic: an incomplete or invalid
package never replaces the currently active version.

The exact release must not already be present in the local cellar.";

const INSTALL_AFTER_HELP: &str = "\
Examples:
  Install the latest stable release:
    pako install vscodium

  Install from another signed release channel:
    pako install intellij-idea --channel beta
";

const UPGRADE_LONG_ABOUT: &str = "\
Upgrade a package using its locally remembered release channel.

The command uses the same transactional pipeline as installation. Existing
chunks are reused from the local object store, only missing pack data is
downloaded, and the new tree is fully verified before it becomes active.

The previously active version remains available as a rollback candidate. Use
`--dry-run` to resolve metadata and print the download plan without downloading
package packs or modifying the installation.";

const UPGRADE_AFTER_HELP: &str = "\
Examples:
  Show the planned download without changing anything:
    pako upgrade vscodium --dry-run

  Upgrade the package:
    pako upgrade vscodium

  Inspect the active version after the upgrade:
    pako status vscodium";

const VERIFY_LONG_ABOUT: &str = "\
Verify the active installation of a package.

Pako loads the installed receipt and the package manifest stored for the active
version. It checks the expected directory structure, symlink targets, file
sizes, file digests, and the final tree digest.

This command performs a full integrity check and can read every installed file.
It does not repair modified or missing content. A verification failure leaves
the installation unchanged.";

const VERIFY_AFTER_HELP: &str = "\
Examples:
  Verify one installed package:
    pako verify intellij-idea";

const ROLLBACK_LONG_ABOUT: &str = "\
Activate a previously installed version of a package.

Without `--to`, Pako selects the most recent version recorded as a rollback
candidate. With `--to`, the specified version must already exist in the local
cellar.

Before changing the active-version symlink, Pako verifies the complete target
tree against its stored manifest. Rollback does not download content from the
network and does not rebuild a missing version.";

const ROLLBACK_AFTER_HELP: &str = "\
Examples:
  Roll back to the most recent retained version:
    pako rollback intellij-idea

  Activate an explicitly retained version:
    pako rollback intellij-idea --to 2026.1-1

  Confirm the active version:
    pako status intellij-idea";

const REMOVE_LONG_ABOUT: &str = "\
Remove an installed package and all of its retained versions.

Pako removes the package-owned launchers, desktop entries, icons, active-version
symlink, versioned cellar directories, stored package manifests, and receipt.

Shared objects and downloaded pack files are not removed by this command because
they may still be useful to other packages or future installations. Removal is
interactive unless the global `--yes` option is supplied.";

const REMOVE_AFTER_HELP: &str = "\
Examples:
  Ask for confirmation before removal:
    pako remove vscodium

  Remove without an interactive prompt:
    pako --yes remove vscodium";

const LIST_LONG_ABOUT: &str = "\
List packages recorded as installed for the current user.

The command reads local receipts only. It does not contact repositories and does
not perform an integrity verification. Human-readable output contains the
package name, active version, release number, and target architecture.";

const LIST_AFTER_HELP: &str = "\
Examples:
  List installed packages:
    pako list";

const STATUS_LONG_ABOUT: &str = "\
Show locally recorded package status.

When a package name is supplied, Pako displays its active version, release, and
target from the installed receipt. Without a package name, the command behaves
like `pako list` and displays all installed receipts.

Status is intentionally fast: it does not access the network and does not hash
the installed tree. Use `pako verify <PACKAGE>` when an integrity check is
required.";

const STATUS_AFTER_HELP: &str = "\
Examples:
  Show one package receipt:
    pako status intellij-idea

  Show all installed packages:
    pako status";

const RECOVER_LONG_ABOUT: &str = "\
Recover interrupted local package transactions.

Pako examines transaction journals left by an interrupted install or upgrade.
Transactions which did not reach the commit phase have their staging directory
removed. Transactions which committed a package tree restore a valid active
version from the new or previous version recorded in the journal.

Journal paths are validated against Pako-managed directories before any cleanup
or activation is attempted. Successfully handled journals are removed.";

const RECOVER_AFTER_HELP: &str = "\
Examples:
  Recover all interrupted transactions:
    pako recover";

/// Pako command-line interface.
#[derive(Debug, Parser)]
#[command(
    name = "pako",
    version,
    about = "Install and manage user-space packages on Linux",
    long_about = ROOT_LONG_ABOUT,
    after_help = ROOT_AFTER_HELP,
    arg_required_else_help = true,
    disable_help_subcommand = true
)]
pub(crate) struct Cli {
    /// Increase diagnostic detail. Repeat for debug and trace output.
    ///
    /// Complete diagnostics are always written to the operation log.
    #[arg(short, long, action = ArgAction::Count, global = true)]
    pub(crate) verbose: u8,

    /// Confirm planned changes without reading from standard input.
    ///
    /// This applies to install, upgrade, rollback, prune, and removal. It has
    /// no effect on read-only commands such as `list`, `status`, and `verify`.
    #[arg(short = 'y', long, global = true)]
    pub(crate) yes: bool,

    /// Maximum number of CPU and filesystem workers.
    ///
    /// The default is the number of logical CPUs reported by the operating
    /// system. Network transfers have a separate, lower default limit.
    #[arg(long, value_name = "JOBS", global = true)]
    pub(crate) jobs: Option<NonZeroUsize>,

    /// Maximum number of concurrent registry blob downloads.
    ///
    /// The default is the smaller of six and the selected CPU worker count.
    #[arg(long, value_name = "JOBS", global = true)]
    pub(crate) download_jobs: Option<NonZeroUsize>,

    #[command(subcommand)]
    pub(crate) command: Command,
}

/// Operations supported by the Pako client.
#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Install a package from a trusted repository.
    #[command(
        long_about = INSTALL_LONG_ABOUT,
        after_help = INSTALL_AFTER_HELP
    )]
    Install(InstallArgs),

    /// Upgrade a package using its remembered release channel.
    #[command(
        long_about = UPGRADE_LONG_ABOUT,
        after_help = UPGRADE_AFTER_HELP
    )]
    Upgrade(UpgradeArgs),

    /// Verify every entry in the active package tree.
    #[command(
        long_about = VERIFY_LONG_ABOUT,
        after_help = VERIFY_AFTER_HELP
    )]
    Verify(VerifyArgs),

    /// Activate a retained, previously installed version.
    #[command(
        long_about = ROLLBACK_LONG_ABOUT,
        after_help = ROLLBACK_AFTER_HELP
    )]
    Rollback(RollbackArgs),

    /// List retained versions of one package.
    #[command(
        long_about = "List every retained local version, with the active version first.",
        after_help = "Examples:\n  pako versions intellij-idea"
    )]
    Versions(VersionsArgs),

    /// Remove older retained versions.
    #[command(
        long_about = "Remove retained versions after the requested keep count. The active version is always retained.",
        after_help = "Examples:\n  pako prune intellij-idea --keep 2"
    )]
    Prune(PruneArgs),

    /// Remove a package and all of its retained versions.
    #[command(
        long_about = REMOVE_LONG_ABOUT,
        after_help = REMOVE_AFTER_HELP
    )]
    Remove(RemoveArgs),

    /// List installed package receipts.
    #[command(long_about = LIST_LONG_ABOUT, after_help = LIST_AFTER_HELP)]
    List,

    /// Show local status for one package or all installed packages.
    #[command(
        long_about = STATUS_LONG_ABOUT,
        after_help = STATUS_AFTER_HELP
    )]
    Status(StatusArgs),

    /// Recover transactions interrupted before completion.
    #[command(
        long_about = RECOVER_LONG_ABOUT,
        after_help = RECOVER_AFTER_HELP
    )]
    Recover,
}

/// Arguments accepted by `pako install`.
#[derive(Debug, Args)]
pub(crate) struct InstallArgs {
    /// Package name from the configured repository catalog.
    #[arg(value_name = "PACKAGE")]
    pub(crate) package: String,

    /// Signed release channel to resolve.
    ///
    /// Channel names are defined by repository metadata. The default channel is
    /// `stable`.
    #[arg(long, value_name = "CHANNEL", default_value = "stable")]
    pub(crate) channel: String,
}

/// Arguments accepted by `pako upgrade`.
#[derive(Debug, Args)]
pub(crate) struct UpgradeArgs {
    /// Installed package to resolve and upgrade.
    #[arg(value_name = "PACKAGE")]
    pub(crate) package: String,

    /// Override the release channel remembered when the package was installed.
    ///
    /// When omitted, Pako reads the channel from the local package state.
    #[arg(long, value_name = "CHANNEL")]
    pub(crate) channel: Option<String>,

    /// Print the resolved download plan without downloading packs or changing
    /// the active installation.
    #[arg(long)]
    pub(crate) dry_run: bool,
}

/// Arguments accepted by `pako verify`.
#[derive(Debug, Args)]
pub(crate) struct VerifyArgs {
    /// Installed package whose active tree should be verified.
    #[arg(value_name = "PACKAGE")]
    pub(crate) package: String,
}

/// Arguments accepted by `pako rollback`.
#[derive(Debug, Args)]
pub(crate) struct RollbackArgs {
    /// Installed package to roll back.
    #[arg(value_name = "PACKAGE")]
    pub(crate) package: String,

    /// Exact retained version to activate, in `<UPSTREAM_VERSION>-<RELEASE>`
    /// form.
    ///
    /// When omitted, Pako selects the most recent rollback candidate recorded
    /// in the package receipt.
    #[arg(long, value_name = "VERSION")]
    pub(crate) to: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct VersionsArgs {
    /// Package whose retained versions should be listed.
    #[arg(value_name = "PACKAGE")]
    pub(crate) package: String,
}

#[derive(Debug, Args)]
pub(crate) struct PruneArgs {
    /// Package whose older versions should be removed.
    #[arg(value_name = "PACKAGE")]
    pub(crate) package: String,
    /// Number of most recent history entries to retain.
    #[arg(long, value_name = "COUNT")]
    pub(crate) keep: NonZeroUsize,
}

/// Arguments accepted by `pako remove`.
#[derive(Debug, Args)]
pub(crate) struct RemoveArgs {
    /// Installed package to remove.
    #[arg(value_name = "PACKAGE")]
    pub(crate) package: String,
}

/// Arguments accepted by `pako status`.
#[derive(Debug, Args)]
pub(crate) struct StatusArgs {
    /// Package whose receipt should be displayed.
    ///
    /// Omit this argument to display all installed packages.
    #[arg(value_name = "PACKAGE")]
    pub(crate) package: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Concurrency {
    pub(crate) cpu_jobs: usize,
    pub(crate) download_jobs: usize,
}

impl Cli {
    pub(crate) fn concurrency(&self) -> Concurrency {
        let cpu_jobs = self.jobs.map_or_else(
            || std::thread::available_parallelism().map_or(1, NonZeroUsize::get),
            NonZeroUsize::get,
        );
        let download_jobs = self
            .download_jobs
            .map_or_else(|| cpu_jobs.min(6), NonZeroUsize::get);
        Concurrency {
            cpu_jobs,
            download_jobs,
        }
    }

    pub(crate) fn operation_log_name(&self) -> String {
        let (operation, package) = match &self.command {
            Command::Install(arguments) => ("install", Some(arguments.package.as_str())),
            Command::Upgrade(arguments) => ("upgrade", Some(arguments.package.as_str())),
            Command::Verify(arguments) => ("verify", Some(arguments.package.as_str())),
            Command::Rollback(arguments) => ("rollback", Some(arguments.package.as_str())),
            Command::Versions(arguments) => ("versions", Some(arguments.package.as_str())),
            Command::Prune(arguments) => ("prune", Some(arguments.package.as_str())),
            Command::Remove(arguments) => ("remove", Some(arguments.package.as_str())),
            Command::List => ("list", None),
            Command::Status(arguments) => ("status", arguments.package.as_deref()),
            Command::Recover => ("recover", None),
        };
        package.map_or_else(
            || operation.to_owned(),
            |package| format!("{operation}-{package}"),
        )
    }

    pub(crate) fn mutates_package_state(&self) -> bool {
        matches!(
            &self.command,
            Command::Install(_)
                | Command::Upgrade(_)
                | Command::Rollback(_)
                | Command::Prune(_)
                | Command::Remove(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use clap::{Command, CommandFactory};

    use super::Cli;

    #[test]
    fn every_command_and_argument_has_help_text() {
        assert_documented(&Cli::command());
    }

    fn assert_documented(command: &Command) {
        if command.get_name() != "pako" {
            assert!(
                command.get_about().is_some(),
                "command `{}` is missing a short description",
                command.get_name()
            );
            assert!(
                command.get_long_about().is_some(),
                "command `{}` is missing detailed help",
                command.get_name()
            );
            assert!(
                command.get_after_help().is_some(),
                "command `{}` is missing examples",
                command.get_name()
            );
        }

        for argument in command.get_arguments() {
            let id = argument.get_id().as_str();
            if matches!(id, "help" | "version") {
                continue;
            }

            assert!(
                argument.get_help().is_some() || argument.get_long_help().is_some(),
                "argument `{id}` in command `{}` is missing help text",
                command.get_name()
            );
        }

        for subcommand in command.get_subcommands() {
            assert_documented(subcommand);
        }
    }
}
