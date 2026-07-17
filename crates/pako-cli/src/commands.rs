use pako_core::{
    installer::Installer,
    layout::Layout,
    receipt::{PackageState, Receipt},
};

use crate::{
    cli::{Cli, Command},
    output::confirm,
    repository::install_remote,
};

pub(crate) async fn run(cli: Cli) -> anyhow::Result<()> {
    let layout = Layout::discover()?;
    layout.ensure()?;
    let installer = Installer::new(layout.clone())?;

    match cli.command {
        Command::Install(arguments) => {
            install_remote(
                &installer,
                &arguments.package,
                &arguments.channel,
                false,
                !cli.yes,
            )
            .await?;
        }
        Command::Upgrade(arguments) => {
            let state = PackageState::load(&layout.package_state(&arguments.package)?)?;
            let channel = arguments.channel.as_deref().unwrap_or(&state.channel);
            install_remote(
                &installer,
                &arguments.package,
                channel,
                arguments.dry_run,
                !cli.yes && !arguments.dry_run,
            )
            .await?;
        }
        Command::Verify(arguments) => {
            let package = arguments.package;
            let report = installer.verify(&package)?;
            println!("{package} is healthy ({} files)", report.files);
        }
        Command::Rollback(arguments) => {
            let package = arguments.package;
            let version = installer.rollback(&package, arguments.to.as_deref())?;
            println!("rolled back {package} to {version}");
        }
        Command::Versions(arguments) => {
            let state = installer.versions(&arguments.package)?;
            for version in state.history {
                println!("{version}");
            }
        }
        Command::Prune(arguments) => {
            let removed = installer.prune(&arguments.package, arguments.keep)?;
            println!("pruned {} retained version(s)", removed.len());
        }
        Command::Remove(arguments) => {
            let package = arguments.package;
            if !cli.yes && !confirm(&format!("Remove {package}?"))? {
                println!("removal cancelled");
                return Ok(());
            }

            installer.remove(&package)?;
            println!("removed {package}");
        }
        Command::List => list_receipts(&layout)?,
        Command::Status(arguments) => {
            status(&layout, arguments.package.as_deref())?;
        }
        Command::Recover => {
            let recovered = pako_core::transaction::recover(&layout)?;
            println!("recovered {} transaction(s)", recovered.len());
        }
    }

    Ok(())
}

fn list_receipts(layout: &Layout) -> anyhow::Result<()> {
    let mut receipts = Vec::new();
    let directory = layout.packages();

    if directory.exists() {
        for entry in std::fs::read_dir(directory)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) == Some("json") {
                let state = PackageState::load(&path)?;
                receipts.push((
                    state.clone(),
                    Receipt::load(&layout.version_record(&state.package, &state.active)?)?,
                ));
            }
        }
    }

    receipts.sort_by(|left, right| left.0.package.cmp(&right.0.package));

    for (state, receipt) in receipts {
        println!(
            "{}\t{}-{}\t{}\t{}",
            receipt.package,
            receipt.upstream_version,
            receipt.release,
            receipt.target,
            state.channel,
        );
    }

    Ok(())
}

fn status(layout: &Layout, package: Option<&str>) -> anyhow::Result<()> {
    let Some(package) = package else {
        return list_receipts(layout);
    };

    let state = PackageState::load(&layout.package_state(package)?)?;
    let receipt = Receipt::load(&layout.version_record(package, &state.active)?)?;
    println!(
        "{} {}-{} ({}, channel {})",
        receipt.package,
        receipt.upstream_version,
        receipt.release,
        receipt.target,
        state.channel,
    );
    Ok(())
}
