use std::io::{self, Write};

use log::{LevelFilter, Log, Metadata, Record};

#[derive(Debug)]
struct Logger;

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let mut stderr = io::stderr().lock();
        let _ = writeln!(stderr, "{:<5} {}", record.level(), record.args());
    }

    fn flush(&self) {}
}

static LOGGER: Logger = Logger;

pub(crate) fn init(verbosity: u8) -> anyhow::Result<()> {
    log::set_logger(&LOGGER).map_err(|_| anyhow::anyhow!("failed to initialize logger"))?;
    log::set_max_level(match verbosity {
        0 => LevelFilter::Info,
        1 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    });
    Ok(())
}
