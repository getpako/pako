use std::{
    ffi::OsStr,
    process::{Command, Stdio},
};

use anyhow::{Context as _, Result};

pub(crate) fn run(command: &mut Command) -> Result<()> {
    let description = describe(command);
    eprintln!("+ {description}");

    let status = command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed to start: {description}"))?;

    if !status.success() {
        anyhow::bail!("command failed with {status}: {description}");
    }

    Ok(())
}

pub(crate) fn succeeds(command: &mut Command) -> bool {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn describe(command: &Command) -> String {
    let program = command.get_program().to_string_lossy();
    let arguments = command
        .get_args()
        .map(OsStr::to_string_lossy)
        .collect::<Vec<_>>()
        .join(" ");

    if arguments.is_empty() {
        program.into_owned()
    } else {
        format!("{program} {arguments}")
    }
}
