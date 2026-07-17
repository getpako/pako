use std::{
    io::{self, IsTerminal as _, Write},
    time::{Duration, Instant},
};

use indicatif::{ProgressBar, ProgressStyle};

#[derive(Debug, Clone, Copy)]
pub(crate) struct Ui {
    assume_yes: bool,
}

impl Ui {
    pub(crate) fn new(assume_yes: bool) -> Self {
        Self { assume_yes }
    }

    pub(crate) fn heading(self, title: &str) {
        println!("{title}");
    }

    pub(crate) fn field(self, label: &str, value: impl std::fmt::Display) {
        println!("  {label:<14} {value}");
    }

    pub(crate) fn blank(self) {
        println!();
    }

    pub(crate) fn note(self, message: impl std::fmt::Display) {
        println!("{message}");
    }

    pub(crate) fn warning(self, message: impl std::fmt::Display) {
        log::warn!("{message}");
    }

    pub(crate) fn confirm(self, prompt: &str) -> anyhow::Result<bool> {
        if self.assume_yes {
            log::debug!("confirmation accepted by --yes: {prompt}");
            return Ok(true);
        }
        if !io::stdin().is_terminal() {
            anyhow::bail!("confirmation requires a terminal; rerun with --yes");
        }

        print!("{prompt} [y/N] ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        Ok(matches!(
            input.trim().to_ascii_lowercase().as_str(),
            "y" | "yes"
        ))
    }

    pub(crate) fn spinner(self, message: impl Into<String>) -> Step {
        let message = message.into();
        log::info!(target: "pako.ui", "{message}");
        let progress = ProgressBar::new_spinner();
        progress.set_style(
            ProgressStyle::with_template("{spinner:.green} {msg}")
                .expect("spinner progress template is valid"),
        );
        progress.set_message(message);
        progress.enable_steady_tick(Duration::from_millis(100));
        Step {
            progress,
            started: Instant::now(),
        }
    }

    pub(crate) fn byte_progress(self, message: impl Into<String>, total: u64) -> ProgressBar {
        let progress = ProgressBar::new(total);
        progress.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} {msg} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})",
            )
            .expect("byte progress template is valid")
            .progress_chars("#>-"),
        );
        progress.set_message(message.into());
        progress.enable_steady_tick(Duration::from_millis(100));
        progress
    }

    pub(crate) fn item_progress(
        self,
        message: impl Into<String>,
        total: usize,
        unit: &str,
    ) -> ProgressBar {
        let progress = ProgressBar::new(total as u64);
        progress.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} {msg} [{bar:40.cyan/blue}] {pos}/{len} {prefix} ({per_sec})",
            )
            .expect("item progress template is valid")
            .progress_chars("#>-"),
        );
        progress.set_message(message.into());
        progress.set_prefix(unit.to_owned());
        progress.enable_steady_tick(Duration::from_millis(100));
        progress
    }
}

#[derive(Debug)]
pub(crate) struct Step {
    progress: ProgressBar,
    started: Instant,
}

impl Step {
    pub(crate) fn finish(self, message: impl Into<String>) {
        let message = message.into();
        let elapsed = self.started.elapsed();
        self.progress.finish_with_message(format!(
            "{message} ({})",
            format_duration(elapsed)
        ));
        log::info!(target: "pako.ui", "{message} in {}", format_duration(elapsed));
    }

    pub(crate) fn abandon(self, message: impl Into<String>) {
        let message = message.into();
        self.progress.abandon_with_message(message.clone());
        log::warn!("{message}");
    }
}

pub(crate) fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;

    if bytes >= GIB {
        format_scaled_size(bytes, GIB, "GiB")
    } else if bytes >= MIB {
        format_scaled_size(bytes, MIB, "MiB")
    } else if bytes >= KIB {
        format_scaled_size(bytes, KIB, "KiB")
    } else {
        format!("{bytes} B")
    }
}

pub(crate) fn format_duration(duration: Duration) -> String {
    if duration.as_secs() > 0 {
        format!("{}.{:01}s", duration.as_secs(), duration.subsec_millis() / 100)
    } else {
        format!("{}ms", duration.as_millis())
    }
}

fn format_scaled_size(bytes: u64, unit: u64, suffix: &str) -> String {
    let whole = bytes / unit;
    let tenths = (bytes % unit) * 10 / unit;
    format!("{whole}.{tenths} {suffix}")
}
