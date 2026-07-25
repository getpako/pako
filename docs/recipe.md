# `recipe.toml` schema 1

A recipe is trusted build-side configuration. It is read only by `pako-build`; the end-user client
installs signed package manifests and never downloads or executes recipe scripts.

The format is intentionally optimized for the common case: one prebuilt archive per architecture,
a few payload adjustments, and optional desktop integration.

## Minimal recipe

```toml
schema = 1
name = "hello-local"
version = "1.0.0"
summary = "Small offline example"
license = "MIT"
executables = ["bin/hello-pako"]

[commands]
hello-pako = "bin/hello-pako"

[source.x86_64]
path = "payload/hello-pako"
to = "bin/hello-pako"
```

`release` defaults to `1`, `display_name` defaults to `name`, and `description` defaults to
`summary`. `vendor` and `homepage` are optional.

A local source does not need an explicit checksum. `pako-build` calculates its SHA-256 digest from
the file stored next to the recipe. Remote sources must always declare `sha256`.

## Architecture-specific sources

The supported keys are `x86_64` and `aarch64`. They are normalized to `linux/x86_64` and
`linux/aarch64` in generated package metadata.

```toml
[source.x86_64]
url = "https://example.org/app-linux-x86_64.tar.gz"
sha256 = "0123456789abcdef..."
strip = 1

[source.aarch64]
url = "https://example.org/app-linux-aarch64.tar.gz"
sha256 = "abcdef0123456789..."
strip = 1
```

Archive formats are inferred from `.tar.gz`, `.tgz`, `.tar`, and `.zip`. Use `format` only when a
URL does not have a useful extension. `mirrors` adds fallback URLs.

A target may contain several sources by using an array of tables:

```toml
[[source.x86_64]]
url = "https://example.org/application.tar.gz"
sha256 = "..."
strip = 1

[[source.x86_64]]
url = "https://example.org/runtime.tar.gz"
sha256 = "..."
strip = 1
```

Sources have no separate ID. For non-archive sources without `to`, Pako uses the filename from
the source URL or local path as the payload filename.

## Payload transformations

Common transformations use compact top-level fields:

```toml
remove = ["Install-Linux-tar.txt"]
executables = ["bin/application"]

[move]
"old/path" = "new/path"

[copy]
"templates/default.conf" = "etc/application.conf"

[permissions]
"bin/helper" = "0750"

[symlinks]
"bin/app" = "../lib/application/app"
```

`remove` is optional by default. `executables` sets mode `0755` and verifies that each path is an
executable file or symlink.

Rare operations may use explicit `[[transform]]` and `[[assertion]]` entries. These retain the
advanced tagged forms implemented by `pako-build` without making normal recipes verbose.

## Commands and desktop integration

A command maps a user-visible launcher name to a path inside the payload:

```toml
[commands]
idea = "bin/idea.sh"
```

Arguments can be declared when needed:

```toml
[commands.idea]
target = "bin/idea.sh"
arguments = ["--disable-update-check"]
```

A desktop entry derives its ID and display name from the package unless they are overridden:

```toml
[desktop]
command = "idea"
arguments = "%F"
icon = "bin/idea.svg"
categories = ["Development", "IDE"]
```

`terminal` defaults to `false`. A payload icon is installed as a scalable application icon. Use
`icon_name` instead of `icon` to reference an existing system icon theme name.

## Source builds

Build scripts live in normal shell files so they can be linted, tested, and reviewed separately
from TOML:

```toml
[build.x86_64]
image = "ghcr.io/getpako/build-images/linux-x86_64-v1@sha256:..."
script = "build.sh"
timeout = 7200
```

The image must be pinned by OCI digest. Network access is disabled unless `network = true` is set.
The script runs with Bash inside the rootless build sandbox and must place the final payload under
`$PAKO_DESTDIR`.

For packages that benefit from separate phases, use file paths under `steps`:

```toml
[build.x86_64]
image = "ghcr.io/getpako/build-images/linux-x86_64-v1@sha256:..."

[build.x86_64.steps]
configure = "scripts/configure.sh"
build = "scripts/build.sh"
check = "scripts/check.sh"
install = "scripts/install.sh"
```

Supported phases are `prepare`, `configure`, `build`, `check`, and `install`. Build scripts and local
sources must remain inside the recipe directory.

## Security properties

- Unknown fields are rejected, so misspelled configuration does not silently change behavior.
- Remote downloads require SHA-256 pinning.
- Build images require immutable OCI digests.
- Archive paths and symlinks are validated before extraction.
- Recipe scripts run only in the build sandbox and are never included in the end-user package.
- The generated package manifest remains the complete source of truth for installation and repair.
