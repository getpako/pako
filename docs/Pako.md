# Pako architecture

Pako is a Linux user-space package manager for large, self-contained
applications. It installs into user-owned XDG directories, verifies the complete
package tree, and changes the active version atomically.

This page is the architecture overview. Use the focused guides for practical
workflows:

- [Using Pako](commands.md) — install, upgrade, verify, rollback, and remove packages.
- [Building packages](recipe.md) — write and validate `recipe.toml` files.
- [Publishing packages](publishing.md) — build artifacts and update TUF metadata.
- [Local development](development.md) — run the TUF server and smoke test.
- [Security model](security.md) — trust boundaries, verification, and limitations.

## System shape

```text
recipe.toml
    │
    ▼
pako-build ──► package-manifest.json + package.tar.zst (hosted only)
    │                         │
    │                         └── TUF targets (publication and trusted resolution)
    │
    ▼
pako CLI ──► signed catalog ──► manifest ──► verified archive ──► atomic activation
```

The package manifest is the source of truth for the installed tree. The signed
TUF catalog resolves a package, channel, and target to the manifest target. The
manifest then identifies either a TUF-hosted archive or an HTTPS external
archive by digest and size.

## Distribution models

`external-archive` keeps the producer's archive outside Pako. The signed
manifest contains every mirror URL, the producer archive's SHA-256 and size,
format, strip count, and optional client-side declarative transforms. The
client downloads and verifies the archive, applies those transforms, and
verifies the resulting tree. Updates currently download the complete archive.

`tuf-archive` stores both manifest and `package.tar.zst` in TUF. The builder
creates the final tree and the manifest has `transforms = []`; the client only
extracts and verifies it.

```text
catalog.json
    ↓
package manifest
    ↓
external URL or TUF archive target
    ↓
cache by SHA-256
    ↓
safe extraction
    ↓
optional external transforms
    ↓
tree verification
    ↓
atomic activation
```

## Workspace components

| Component | Responsibility |
| --- | --- |
| `pako-core` | Package manifests, safe archive extraction, verification, transactions, XDG layout, integrations, and receipts. |
| `pako-cli` | End-user commands, repository resolution, downloads, prompts, and output. |
| `pako-build` | Recipe validation, source preparation, builds, archive creation, and TUF publication. |
| `pako-trust` | Trusted TUF metadata loading and catalog resolution. |
| `pako-log` | Shared logs and progress rendering. |
| `pako-test-support` | Isolated layouts and deterministic test helpers. |
| `xtask` | Development-only environment and smoke-test commands. |

Supported package targets are currently `linux/x86_64` and `linux/aarch64`.

## Install lifecycle

Every install and upgrade follows the same sequence:

1. Refresh and verify TUF metadata.
2. Resolve the requested channel and host target.
3. Read and validate the signed package manifest.
4. Download the selected archive to a temporary `.partial` file.
5. Verify its digest and size before extraction.
6. Extract into a private staging directory.
7. Verify every declared entry and the final tree digest.
8. Preflight launcher, desktop-entry, and icon integrations.
9. Rename the verified tree into its immutable version directory.
10. Atomically replace the `current` symlink and publish integrations.
11. Save the receipt and transaction state.

The active version is never modified in place. Interrupted operations leave a
journal that package-mutating commands recover before continuing.

## Local state

With ordinary XDG defaults, Pako stores:

```text
$XDG_DATA_HOME/pako/
  cellar/<package>/<version>/       immutable installed trees
  apps/<package>/current             active-version symlink
  manifests/<package>/<version>.json stored manifests
  staging/                           transaction staging

$XDG_STATE_HOME/pako/
  packages/<package>.json            active version and channel
  versions/<package>/<version>.json   immutable receipt
  transactions/<id>.json             recovery journal
  locks/                             package and integration locks

$XDG_CACHE_HOME/pako/artifacts/sha256/ downloaded archives by digest
$XDG_CONFIG_HOME/pako/               repository configuration
```

Launchers are exposed below `~/.local/bin`; desktop entries and icons use the
user XDG data directories. Pako does not write to `/usr` and does not require
`sudo` for normal package operations.
