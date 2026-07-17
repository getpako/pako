use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use log::{Level, LevelFilter, Log, Metadata, Record};

#[derive(Debug, Clone)]
pub struct LogHandle {
    path: PathBuf,
}

impl LogHandle {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug)]
struct PakoLogger {
    terminal_level: LevelFilter,
    file: Mutex<File>,
}

impl Log for PakoLogger {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let timestamp = unix_timestamp();
        if let Ok(mut file) = self.file.lock() {
            let _ = writeln!(
                file,
                "[{timestamp}] {:<5} {}: {}",
                record.level(),
                record.target(),
                record.args()
            );
        }

        if record.target() != "pako.ui" && record.level() <= self.terminal_level {
            let prefix = match record.level() {
                Level::Error => "error",
                Level::Warn => "warning",
                Level::Info => "info",
                Level::Debug => "debug",
                Level::Trace => "trace",
            };
            let mut stderr = io::stderr().lock();
            let _ = writeln!(stderr, "{prefix}: {}", record.args());
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock() {
            let _ = file.flush();
        }
        let _ = io::stderr().flush();
    }
}

pub fn init(
    log_directory: &Path,
    operation_name: &str,
    verbosity: u8,
) -> anyhow::Result<LogHandle> {
    std::fs::create_dir_all(log_directory)?;
    let operation_name = sanitize_name(operation_name);
    let path = log_directory.join(format!(
        "{operation_name}-{}.log",
        unix_timestamp()
    ));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    let terminal_level = match verbosity {
        0 => LevelFilter::Warn,
        1 => LevelFilter::Info,
        2 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    };
    let logger = Box::leak(Box::new(PakoLogger {
        terminal_level,
        file: Mutex::new(file),
    }));
    log::set_logger(logger).map_err(|_| anyhow::anyhow!("failed to initialize Pako logger"))?;
    log::set_max_level(LevelFilter::Trace);
    log::info!(target: "pako", "operation log: {}", path.display());

    Ok(LogHandle { path })
}

fn sanitize_name(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    let trimmed = sanitized.trim_matches('-');
    if trimmed.is_empty() {
        "operation".into()
    } else {
        trimmed.chars().take(96).collect()
    }
}

fn unix_timestamp() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
