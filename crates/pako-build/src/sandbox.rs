use std::{collections::BTreeMap, path::Path, process::Stdio, time::Duration};

use tokio::{process::Command, time::timeout};

/// Rootless OCI build sandbox used for source recipes.
#[derive(Debug, Clone)]
pub(crate) struct Sandbox {
    pub image: String,
    pub network: bool,
    pub timeout: Duration,
    pub shell: String,
}

impl Sandbox {
    #[expect(
        clippy::too_many_arguments,
        reason = "the mount paths and environment are explicit sandbox inputs"
    )]
    pub(crate) async fn run(
        &self,
        phase: &str,
        script: &str,
        recipe: &Path,
        source: &Path,
        build: &Path,
        destination: &Path,
        environment: &BTreeMap<String, String>,
    ) -> anyhow::Result<()> {
        log::info!("running {phase} build phase in {}", self.image);
        log::info!("sandbox network access: {}", self.network);
        let mut command = Command::new("podman");
        command.args([
            "run",
            "--rm",
            "--userns=keep-id",
            "--security-opt=no-new-privileges",
            "--cap-drop=all",
            "--read-only",
        ]);

        if !self.network {
            command.args(["--network", "none"]);
        }

        command.arg("--tmpfs=/tmp:rw,noexec,nosuid,nodev,size=1g");
        add_volume(&mut command, recipe, "/pako/recipe", true);
        add_volume(&mut command, source, "/pako/source", false);
        add_volume(&mut command, build, "/pako/build", false);
        add_volume(&mut command, destination, "/pako/dest", false);

        for (key, value) in environment {
            command.arg("--env").arg(format!("{key}={value}"));
        }

        command
            .arg(&self.image)
            .args([self.shell.as_str(), "-euo", "pipefail", "-c", script])
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        let status = timeout(self.timeout, command.status())
            .await
            .map_err(|_| anyhow::anyhow!("build phase `{phase}` timed out"))??;

        if !status.success() {
            anyhow::bail!("build phase `{phase}` failed with {status}");
        }

        Ok(())
    }
}

fn add_volume(command: &mut Command, source: &Path, target: &str, read_only: bool) {
    let mode = if read_only { "ro" } else { "rw" };
    command
        .arg("--volume")
        .arg(format!("{}:{target}:{mode}", source.display()));
}
