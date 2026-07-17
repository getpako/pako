use std::path::Path;

use pako_core::{
    installer::Installer,
    layout::Layout,
    receipt::{PackageState, Receipt},
};
use walkdir::WalkDir;

use crate::{
    cli::{Cli, Command},
    output::{format_size, Ui},
    repository::{
        execute_remote, resolve_remote, InstallOutcome, PackageOperation, RemoteInstallPlan,
    },
};

#[allow(clippy::too_many_lines)]
pub(crate) async fn run(cli: Cli) -> anyhow::Result<()> {
    let layout = Layout::discover()?;
    layout.ensure()?;
    let concurrency = cli.concurrency();
    let installer = Installer::with_jobs(layout.clone(), concurrency.cpu_jobs)?;
    let ui = Ui::new(cli.yes);

    if cli.mutates_package_state() {
        let recovery_layout = layout.clone();
        let recovered =
            tokio::task::spawn_blocking(move || pako_core::transaction::recover(&recovery_layout))
                .await??;
        if !recovered.is_empty() {
            ui.warning(format!(
                "recovered {} interrupted transaction(s) before continuing",
                recovered.len()
            ));
        }
    }

    match cli.command {
        Command::Install(arguments) => {
            install_or_upgrade(
                &installer,
                &layout,
                &arguments.package,
                &arguments.channel,
                PackageOperation::Install,
                false,
                concurrency,
                ui,
            )
            .await?;
        }
        Command::Upgrade(arguments) => {
            let state = PackageState::load(&layout.package_state(&arguments.package)?)?;
            let channel = arguments.channel.as_deref().unwrap_or(&state.channel);
            install_or_upgrade(
                &installer,
                &layout,
                &arguments.package,
                channel,
                PackageOperation::Upgrade,
                arguments.dry_run,
                concurrency,
                ui,
            )
            .await?;
        }
        Command::Verify(arguments) => {
            let package = arguments.package;
            let step = ui.spinner(format!("Verifying {package}"));
            let local_installer = installer.clone();
            let verify_package = package.clone();
            let report =
                tokio::task::spawn_blocking(move || local_installer.verify(&verify_package))
                    .await??;
            step.finish(format!(
                "Verified {package}: {} files, {} directories, {} symlinks",
                report.files, report.directories, report.symlinks
            ));
        }
        Command::Rollback(arguments) => {
            let package = arguments.package;
            let state = PackageState::load(&layout.package_state(&package)?)?;
            let target = arguments
                .to
                .clone()
                .or_else(|| {
                    state
                        .history
                        .iter()
                        .find(|version| *version != &state.active)
                        .cloned()
                })
                .ok_or_else(|| anyhow::anyhow!("no rollback version available"))?;
            ui.heading("Rollback plan");
            ui.field("Package", &package);
            ui.field("Current", &state.active);
            ui.field("Target", &target);
            ui.blank();
            if !ui.confirm("Proceed with rollback?")? {
                ui.note("Rollback cancelled");
                return Ok(());
            }
            let step = ui.spinner(format!("Rolling back {package}"));
            let local_installer = installer.clone();
            let rollback_package = package.clone();
            let rollback_target = target.clone();
            let version = tokio::task::spawn_blocking(move || {
                local_installer.rollback(&rollback_package, Some(&rollback_target))
            })
            .await??;
            step.finish(format!("Activated {package} {version}"));
        }
        Command::Versions(arguments) => {
            let state = installer.versions(&arguments.package)?;
            for version in state.history {
                let marker = if version == state.active { "*" } else { " " };
                pako_log::suspend_progress(|| println!("{marker} {version}"));
            }
        }
        Command::Prune(arguments) => {
            let package = arguments.package;
            let keep = arguments.keep.get();
            let state = PackageState::load(&layout.package_state(&package)?)?;
            let removed = state.history.iter().skip(keep).cloned().collect::<Vec<_>>();
            if removed.is_empty() {
                ui.note(format!("No retained versions of {package} need pruning"));
                return Ok(());
            }
            let size_layout = layout.clone();
            let size_package = package.clone();
            let size_versions = removed.clone();
            let reclaimed = tokio::task::spawn_blocking(move || {
                retained_versions_size(&size_layout, &size_package, &size_versions)
            })
            .await??;
            ui.heading("Prune plan");
            ui.field("Package", &package);
            ui.field("Remove", format!("{} version(s)", removed.len()));
            ui.field("Reclaim", format_size(reclaimed));
            for version in &removed {
                ui.field("Version", version);
            }
            ui.blank();
            if !ui.confirm("Proceed with pruning?")? {
                ui.note("Prune cancelled");
                return Ok(());
            }
            let step = ui.spinner(format!("Pruning retained versions of {package}"));
            let local_installer = installer.clone();
            let prune_package = package.clone();
            let removed =
                tokio::task::spawn_blocking(move || local_installer.prune(&prune_package, keep))
                    .await??;
            step.finish(format!("Pruned {} retained version(s)", removed.len()));
        }
        Command::Remove(arguments) => {
            let package = arguments.package;
            let state = PackageState::load(&layout.package_state(&package)?)?;
            let receipt = Receipt::load(&layout.version_record(&package, &state.active)?)?;
            let package_directory = layout.cellar().join(&package);
            let installed_bytes =
                tokio::task::spawn_blocking(move || directory_size(&package_directory)).await??;
            ui.heading("Removal plan");
            ui.field("Package", &package);
            ui.field("Active", &state.active);
            ui.field("Versions", state.history.len());
            ui.field("Installed data", format_size(installed_bytes));
            ui.field("Integrations", receipt.exposures.len());
            ui.field("Shared cache", "will be retained");
            ui.blank();
            if !ui.confirm("Proceed with removal?")? {
                ui.note("Removal cancelled");
                return Ok(());
            }
            let step = ui.spinner(format!("Removing {package}"));
            let local_installer = installer.clone();
            let remove_package = package.clone();
            tokio::task::spawn_blocking(move || local_installer.remove(&remove_package)).await??;
            step.finish(format!("Removed {package}"));
        }
        Command::List => list_receipts(&layout)?,
        Command::Status(arguments) => {
            status(&layout, arguments.package.as_deref())?;
        }
        Command::Recover => {
            let step = ui.spinner("Recovering interrupted transactions");
            let recovery_layout = layout.clone();
            let recovered = tokio::task::spawn_blocking(move || {
                pako_core::transaction::recover(&recovery_layout)
            })
            .await??;
            step.finish(format!("Recovered {} transaction(s)", recovered.len()));
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn install_or_upgrade(
    installer: &Installer,
    layout: &Layout,
    package: &str,
    channel: &str,
    operation: PackageOperation,
    dry_run: bool,
    concurrency: crate::cli::Concurrency,
    ui: Ui,
) -> anyhow::Result<()> {
    let plan = resolve_remote(installer, package, channel, operation, concurrency, ui).await?;
    print_remote_plan(&plan, layout, ui)?;

    if plan.up_to_date {
        ui.blank();
        ui.note(format!(
            "{} {} is already up to date",
            plan.manifest.package,
            plan.version()
        ));
        return Ok(());
    }
    if dry_run {
        ui.blank();
        ui.note("Dry run complete; no changes were made");
        return Ok(());
    }

    ui.blank();
    if !ui.confirm("Proceed with this operation?")? {
        ui.note("Operation cancelled");
        return Ok(());
    }
    ui.blank();

    match execute_remote(installer, plan, ui).await? {
        InstallOutcome::Installed(receipt) => {
            ui.note(format!(
                "Installed {} {}-{}",
                receipt.package, receipt.upstream_version, receipt.release
            ));
        }
        InstallOutcome::AlreadyCurrent => {
            ui.note("Package is already up to date");
        }
    }
    Ok(())
}

fn print_remote_plan(plan: &RemoteInstallPlan, layout: &Layout, ui: Ui) -> anyhow::Result<()> {
    ui.blank();
    ui.heading(match plan.operation {
        PackageOperation::Install => "Installation plan",
        PackageOperation::Upgrade => "Upgrade plan",
    });
    ui.field("Package", &plan.manifest.package);
    ui.field("Repository", &plan.repository);
    ui.field("Channel", &plan.channel);
    if let Some(current) = &plan.current_version {
        ui.field("Current", current);
    }
    ui.field("Version", plan.version());
    ui.field("Target", &plan.target);
    ui.field(
        "Download",
        format!(
            "{} in {} pack(s)",
            format_size(plan.download.network_bytes),
            plan.download.packs_to_download()
        ),
    );
    if plan.download.cached_packs() > 0 {
        ui.field(
            "Pack cache",
            format!("{} verified pack(s)", plan.download.cached_packs()),
        );
    }
    ui.field(
        "Local reuse",
        format!(
            "{} ({} of {} chunks)",
            format_size(plan.reusable_bytes),
            plan.available_chunks,
            plan.total_chunks
        ),
    );
    ui.field("Installed", format_size(plan.installed_bytes));
    ui.field("Data growth", format_size(plan.data_growth()));
    ui.field("Cache growth", format_size(plan.cache_growth()));
    ui.field(
        "Free space",
        format!(
            "{} data, {} cache",
            format_size(fs2::available_space(&layout.data)?),
            format_size(fs2::available_space(&layout.cache)?)
        ),
    );
    ui.field(
        "Integrations",
        format!(
            "{} launcher(s), {} desktop entry(s), {} icon(s)",
            plan.launcher_count, plan.desktop_entry_count, plan.icon_count
        ),
    );
    if plan.current_version.is_some() {
        ui.field("Rollback", "previous version will be retained");
    }
    if plan.download.overfetch_bytes() > 0 {
        ui.field(
            "Pack overfetch",
            format_size(plan.download.overfetch_bytes()),
        );
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
        pako_log::suspend_progress(|| {
            println!(
                "{}\t{}-{}\t{}\t{}",
                receipt.package,
                receipt.upstream_version,
                receipt.release,
                receipt.target,
                state.channel,
            );
        });
    }
    Ok(())
}

fn status(layout: &Layout, package: Option<&str>) -> anyhow::Result<()> {
    let Some(package) = package else {
        return list_receipts(layout);
    };

    let state = PackageState::load(&layout.package_state(package)?)?;
    let receipt = Receipt::load(&layout.version_record(package, &state.active)?)?;
    pako_log::suspend_progress(|| {
        println!(
            "{} {}-{} ({}, channel {})",
            receipt.package,
            receipt.upstream_version,
            receipt.release,
            receipt.target,
            state.channel,
        );
    });
    Ok(())
}

fn retained_versions_size(
    layout: &Layout,
    package: &str,
    versions: &[String],
) -> anyhow::Result<u64> {
    versions.iter().try_fold(0_u64, |total, version| {
        let size = directory_size(&layout.package_version(package, version)?)?;
        total
            .checked_add(size)
            .ok_or_else(|| anyhow::anyhow!("size overflow"))
    })
}

fn directory_size(path: &Path) -> anyhow::Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .try_fold(0_u64, |total, entry| {
            let entry = entry?;
            if entry.file_type().is_file() {
                total
                    .checked_add(entry.metadata()?.len())
                    .ok_or_else(|| anyhow::anyhow!("directory size overflow"))
            } else {
                Ok(total)
            }
        })
}
