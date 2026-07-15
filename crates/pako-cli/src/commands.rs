use pako_core::{
    installer::Installer,
    layout::Layout,
    receipt::{PackageState, Receipt},
};

use crate::{
    cli::{Cli, Command},
    output::{confirm, Output},
    repository::install_remote,
};

pub(crate) async fn run(cli: Cli) -> anyhow::Result<()> {
    let layout = Layout::discover()?;
    layout.ensure()?;
    let installer = Installer::new(layout.clone())?;
    let output = Output::new(cli.json);

    match cli.command {
        Command::Install(arguments) => {
            install_remote(&installer, &arguments.package, &arguments.channel, false).await?;
        }
        Command::Upgrade(arguments) => {
            install_remote(&installer, &arguments.package, "stable", arguments.dry_run).await?;
        }
        Command::Verify(arguments) => {
            let package = arguments.package;
            let report = installer.verify(&package)?;
            let json = serde_json::json!({
                "package": package,
                "status": "healthy",
                "files": report.files,
                "directories": report.directories,
                "symlinks": report.symlinks,
                "treeDigest": report.tree_digest,
            });
            output.print(
                &json,
                format!("{package} is healthy ({} files)", report.files),
            )?;
        }
        Command::Rollback(arguments) => {
            let package = arguments.package;
            let version = installer.rollback(&package, arguments.to.as_deref())?;
            let json = serde_json::json!({
                "package": package,
                "version": version,
            });
            output.print(&json, format!("rolled back {package} to {version}"))?;
        }
        Command::Versions(arguments) => {
            let state = installer.versions(&arguments.package)?;
            output.print(&serde_json::to_value(&state)?, state.history.join("\n"))?;
        }
        Command::Prune(arguments) => {
            let removed = installer.prune(&arguments.package, arguments.keep)?;
            output.print(
                &serde_json::json!({"package": arguments.package, "removed": removed}),
                "pruned retained versions",
            )?;
        }
        Command::Remove(arguments) => {
            let package = arguments.package;
            if !cli.yes && !confirm(&format!("Remove {package}?"))? {
                return Ok(());
            }

            installer.remove(&package)?;
            let json = serde_json::json!({
                "package": package,
                "removed": true,
            });
            output.print(&json, format!("removed {package}"))?;
        }
        Command::List => list_receipts(output, &layout)?,
        Command::Status(arguments) => {
            status(output, &layout, arguments.package.as_deref())?;
        }
        Command::Recover => {
            let recovered = pako_core::transaction::recover(&layout)?;
            let recovered_count = recovered.len();
            let json = serde_json::json!({ "recovered": recovered });
            output.print(&json, format!("recovered {recovered_count} transaction(s)"))?;
        }
    }

    Ok(())
}

fn list_receipts(output: Output, layout: &Layout) -> anyhow::Result<()> {
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

    if output.is_json() {
        println!("{}", serde_json::to_string_pretty(&receipts)?);
    } else {
        for (_, receipt) in receipts {
            println!(
                "{}\t{}-{}\t{}",
                receipt.package, receipt.upstream_version, receipt.release, receipt.target,
            );
        }
    }

    Ok(())
}

fn status(output: Output, layout: &Layout, package: Option<&str>) -> anyhow::Result<()> {
    let Some(package) = package else {
        return list_receipts(output, layout);
    };

    let state = PackageState::load(&layout.package_state(package)?)?;
    let receipt = Receipt::load(&layout.version_record(package, &state.active)?)?;
    let json = serde_json::json!({ "state": state, "version": receipt });
    output.print(
        &json,
        format!(
            "{} {}-{} ({})",
            receipt.package, receipt.upstream_version, receipt.release, receipt.target
        ),
    )
}
