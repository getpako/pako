mod archive;
mod builder;
mod logging;
mod publisher;
mod recipe;
mod sandbox;
mod tuf;

use std::{num::NonZeroUsize, path::PathBuf};

use clap::{ArgAction, Args, Parser, Subcommand};

const ROOT_LONG_ABOUT: &str = "\
Build Pako package artifacts from a versioned recipe.

`pako-build` is a maintainer tool. It validates recipe files, fetches and
verifies pinned sources, prepares a package payload for one target, audits the
result, divides regular files into content-defined chunks, and writes immutable
packfiles plus the generated package manifest and pack index.

Build scripts from source recipes run only in the configured build sandbox.
They are never included in, or executed by, the end-user Pako client.";

const ROOT_AFTER_HELP: &str = "\
Typical workflow:
  pako-build lint packages/vscodium/recipe.toml
  pako-build build packages/vscodium/recipe.toml \\
    --target linux/x86_64 \\
    --output build/vscodium

Run `pako-build <COMMAND> --help` for detailed command documentation.";

const LINT_LONG_ABOUT: &str = "\
Parse and validate a Pako recipe without downloading sources or building a
payload.

Validation checks the recipe schema, package identity, target definitions,
source declarations, SHA-256 values, architecture-specific configuration,
integrations, transforms, assertions, and build-stage configuration.

This command is intended for local development and continuous integration. A
successful result proves that the recipe is structurally valid, not that its
remote sources are available or that the package can be built successfully.";

const LINT_AFTER_HELP: &str = "\
Examples:
  Validate a prebuilt package recipe:
    pako-build lint packages/intellij-idea/recipe.toml

  Validate a source-build recipe:
    pako-build lint packages/vscodium/recipe.toml";

const BUILD_LONG_ABOUT: &str = "\
Build one architecture-specific target from a Pako recipe.

For a prebuilt target, the builder downloads the target-specific source,
verifies its declared digest, safely extracts it, applies the
configured transforms, and validates the resulting payload.

For a source target, the builder prepares pinned sources and executes declared
prepare, configure, build, check, and install stages inside the configured
sandbox. The install stage must place the final payload in `PAKO_DESTDIR`.

After payload validation, the builder creates deterministic package metadata,
content-defined chunks, immutable packfiles, and a pack index in the selected
output directory. This command does not publish artifacts to a registry.";

const BUILD_AFTER_HELP: &str = "\
Examples:
  Build the x86_64 target in the default `build` directory:
    pako-build build packages/intellij-idea/recipe.toml \\
      --target linux/x86_64

  Build the ARM64 target into a package-specific directory:
    pako-build build packages/intellij-idea/recipe.toml \\
      --target linux/aarch64 \\
      --output build/intellij-idea-aarch64

Generated files include:
  package-manifest.json
  pack-index.json
  build-report.json
  packs/*.pakopack";

/// Command-line interface for package maintainers.
#[derive(Debug, Parser)]
#[command(
    name = "pako-build",
    version,
    about = "Validate recipes and build Pako package artifacts",
    long_about = ROOT_LONG_ABOUT,
    after_help = ROOT_AFTER_HELP,
    arg_required_else_help = true,
    disable_help_subcommand = true
)]
struct Cli {
    /// Increase log detail. Repeat for trace-level diagnostics.
    #[arg(short, long, action = ArgAction::Count, global = true)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

/// Maintainer operations supported by `pako-build`.
#[derive(Debug, Subcommand)]
enum Command {
    /// Validate a recipe without fetching or building anything.
    #[command(long_about = LINT_LONG_ABOUT, after_help = LINT_AFTER_HELP)]
    Lint(LintArgs),

    /// Build one target into manifests and immutable packfiles.
    #[command(long_about = BUILD_LONG_ABOUT, after_help = BUILD_AFTER_HELP)]
    Build(BuildArgs),

    /// Publish verified build artifacts as an OCI image index.
    #[command(long_about = PUBLISH_LONG_ABOUT, after_help = PUBLISH_AFTER_HELP)]
    Publish(PublishArgs),

    /// Create a local TUF repository for development or CI signing.
    #[command(long_about = TUF_LONG_ABOUT, after_help = TUF_AFTER_HELP)]
    Tuf(TufArgs),
}

/// Arguments accepted by `pako-build lint`.
#[derive(Debug, Args)]
struct LintArgs {
    /// Path to the `recipe.toml` file to parse and validate.
    #[arg(value_name = "RECIPE")]
    recipe: PathBuf,
}

/// Arguments accepted by `pako-build build`.
#[derive(Debug, Args)]
struct BuildArgs {
    /// Path to the `recipe.toml` file which defines the package.
    #[arg(value_name = "RECIPE")]
    recipe: PathBuf,

    /// Exact target from the recipe to build.
    ///
    /// Pako 0.1.0 recipes normally use `linux/x86_64` or `linux/aarch64`.
    #[arg(long, value_name = "TARGET")]
    target: String,

    /// Directory in which generated manifests, reports, and packfiles are
    /// written.
    #[arg(long, value_name = "DIRECTORY", default_value = "build")]
    output: PathBuf,

    /// Maximum number of packs to compress concurrently. Defaults to all
    /// available CPUs.
    #[arg(long, value_name = "JOBS")]
    jobs: Option<NonZeroUsize>,
}

const PUBLISH_LONG_ABOUT: &str = "\
Upload a verified pako-build artifact directory to an OCI registry.

The command verifies the package manifest, pack index, and every immutable pack
before uploading. It publishes an OCI platform manifest by digest and then an
OCI image index at the requested tag. It then updates and signs `catalog.json`
in the supplied local TUF repository.";

const PUBLISH_AFTER_HELP: &str = "\
Examples:
  pako-build publish build/hello-local/hello-local/1.0.0-1/linux_x86_64 \\
    --reference registry.example.org/pako/hello-local:1.0.0-1-linux-x86_64 \\
    --tuf /srv/pako/tuf

  pako-build publish build/hello-local/hello-local/1.0.0-1/linux_x86_64 \\
    --reference localhost:5000/pako/hello-local:dev --insecure-http \\
    --tuf /tmp/pako-tuf-dev";

/// Arguments accepted by `pako-build publish`.
#[derive(Debug, Args)]
struct PublishArgs {
    /// Directory created for one target by `pako-build build`.
    #[arg(value_name = "ARTIFACT")]
    artifact: PathBuf,

    /// Local TUF repository to update after OCI publication.
    #[arg(long, value_name = "DIRECTORY")]
    tuf: PathBuf,

    /// OCI repository and tag to update.
    #[arg(long, value_name = "REFERENCE")]
    reference: pako_oci::OciReference,

    /// Use HTTP rather than HTTPS; intended only for a local development registry.
    #[arg(long)]
    insecure_http: bool,

    /// OCI basic-auth username; may also be supplied as `PAKO_OCI_USERNAME`.
    #[arg(long, env = "PAKO_OCI_USERNAME", requires = "password")]
    username: Option<String>,

    /// OCI basic-auth password; may also be supplied as `PAKO_OCI_PASSWORD`.
    #[arg(
        long,
        env = "PAKO_OCI_PASSWORD",
        requires = "username",
        hide_env_values = true
    )]
    password: Option<String>,
}

#[derive(Debug, Args)]
struct TufArgs {
    #[command(subcommand)]
    command: TufCommand,
}

const TUF_LONG_ABOUT: &str = "\
Create and maintain signed TUF metadata consumed by Pako clients.

The generated local repository uses one development key and is intended for
integration tests only. Production deployments must use separate role keys in
a dedicated signing system.";

const TUF_AFTER_HELP: &str = "\
Example:
  pako-build tuf init /srv/pako/tuf";

#[derive(Debug, Subcommand)]
enum TufCommand {
    /// Initialize a single-key TUF repository for local development.
    #[command(
        long_about = "Initialize a single-key TUF repository for local development.\n\
\n\
The command generates an Ed25519 key, writes an empty catalog, and creates
root, targets, snapshot, and timestamp metadata.",
        after_help = "The generated private key is for local development only; do not use it for production."
    )]
    Init {
        /// New directory in which metadata, targets, and the development key are created.
        #[arg(value_name = "DIRECTORY")]
        directory: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    logging::init(cli.verbose)?;

    match cli.command {
        Command::Lint(arguments) => {
            log::info!("validating recipe {}", arguments.recipe.display());
            let recipe = recipe::Recipe::load(&arguments.recipe)?;
            recipe.validate()?;
            println!(
                "recipe is valid: {} {}",
                recipe.package.name, recipe.package.version
            );
        }
        Command::Build(arguments) => {
            log::info!(
                "building recipe {} for {}",
                arguments.recipe.display(),
                arguments.target
            );
            let recipe = recipe::Recipe::load(&arguments.recipe)?;
            let jobs = arguments.jobs.map_or_else(
                || std::thread::available_parallelism().map_or(1, NonZeroUsize::get),
                NonZeroUsize::get,
            );
            let report = builder::Builder::new(arguments.output, jobs)?
                .build(&recipe, &arguments.target)
                .await?;

            println!(
                "built {} {} for {}",
                report.package, report.version, report.target
            );
            println!("output: {}", report.output.display());
            println!("manifest: {}", report.package_manifest.display());
            println!("pack index: {}", report.pack_index.display());
        }
        Command::Publish(arguments) => {
            log::info!(
                "publishing artifact {} to {}",
                arguments.artifact.display(),
                arguments.reference
            );
            let credentials = arguments.username.zip(arguments.password);
            let reference = arguments.reference.clone();
            let digest = publisher::publish(
                &arguments.artifact,
                reference.clone(),
                arguments.insecure_http,
                credentials,
            )
            .await?;
            let artifact = std::fs::read(arguments.artifact.join("package-manifest.json"))?;
            let manifest: pako_core::PackageManifest = serde_json::from_slice(&artifact)?;
            log::info!("updating signed TUF catalog");
            tuf::add_release(
                &arguments.tuf,
                manifest.package.clone(),
                tuf::release(
                    manifest.upstream_version,
                    manifest.release,
                    manifest.target,
                    reference.to_string(),
                    digest,
                ),
            )
            .await?;
            println!("published OCI image index: {digest}");
            println!("updated signed TUF catalog: {}", arguments.tuf.display());
        }
        Command::Tuf(arguments) => match arguments.command {
            TufCommand::Init { directory } => {
                log::info!("initializing TUF repository {}", directory.display());
                tuf::init(&directory).await?;
                println!("initialized local TUF repository: {}", directory.display());
            }
        },
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::{Command as ClapCommand, CommandFactory};

    use super::Cli;

    #[test]
    fn every_command_and_argument_has_help_text() {
        assert_documented(&Cli::command());
    }

    fn assert_documented(command: &ClapCommand) {
        if command.get_name() != "pako-build" {
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
