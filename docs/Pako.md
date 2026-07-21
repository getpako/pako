# Pako architecture

Pako is a Linux user-space package manager for large, self-contained applications.
It installs only into user-owned XDG locations, does not require `sudo`, and does
not manage system packages, services, drivers, or files under `/usr`.

This document describes the implemented `0.1.0` architecture. Pako uses a complete,
verified payload archive for every release. It has no shared chunk store, packfile
format, or delta-download protocol.

## Design goals and invariants

The architecture is built around a small set of rules:

- A release is identified by immutable SHA-256 digests, never by a mutable OCI tag
  or URL.
- The package manifest is the complete source of truth for the payload tree and
  user-facing integrations.
- Pako verifies a downloaded archive before extracting it, and verifies every
  extracted entry and the tree digest before activation.
- A release is installed in a new versioned directory; an active directory is
  never modified in place.
- The `current` symlink changes only after the new tree is complete and verified.
- Interrupted changes are journaled and recovered to a complete old or new state.
- Client-side installation never executes package-provided scripts.
- A package may not overwrite an integration owned by another package or an
  unmanaged user file.

Supported package targets are `linux/x86_64` and `linux/aarch64`.

## Components

```text
recipe.toml
    │
    ▼
pako-build ──► package-manifest.json + payload.tar.zst ──► OCI registry
    │                                                        │
    └────────────────► signed TUF catalog.json ◄─────────────┘
                                                             │
                                                             ▼
                                                        pako CLI
                                                             │
                                                             ▼
                                              pako-core installer + XDG layout
```

| Component | Responsibility |
| --- | --- |
| `pako-core` | Registry-independent package model, safe extraction, verification, XDG layout, local transactions, integrations, receipts, and lifecycle operations. |
| `pako-cli` | Command parsing, user interaction, progress, repository configuration, TUF resolution, OCI downloads, and wiring to `pako-core`. |
| `pako-build` | Recipe validation, source preparation, optional sandboxed builds, payload creation, manifest generation, OCI publication, and development TUF catalog updates. |
| `pako-oci` | Native OCI Distribution API client for manifest and blob transfer; it has no install-domain logic. |
| `pako-trust` | Loads trusted TUF metadata and resolves a package/channel/target to an immutable OCI manifest digest. |
| `pako-log` | Shared logging and progress coordination so low-level transfers do not create competing terminal output. |
| `pako-test-support` | Isolated layouts and deterministic test fixtures. |
| `xtask` | Development tooling, outside the end-user runtime path. |

## Release format

Each release consists of two OCI layers:

1. `package-manifest.json` with media type
   `application/vnd.pako.package-manifest.v1+json`.
2. `payload.tar.zst` with media type
   `application/vnd.pako.payload.v1+tar+zstd`.

The publisher wraps these layers in an OCI image manifest. It publishes that
platform-specific manifest by digest and publishes an OCI image index at the
requested tag. The tag is a publishing/discovery convenience; the signed catalog
records the immutable platform manifest digest used by the client.

### Package manifest

The schema-1 manifest is canonical JSON. It records:

- package name, upstream version, Pako release number, and target;
- display metadata such as summary, vendor, homepage, and license;
- payload media type, SHA-256 digest, and exact byte size;
- a sorted list of directory, regular-file, and symlink entries;
- each regular file's normalized mode, size, and SHA-256 digest;
- each directory's normalized mode and each symlink's target;
- a SHA-256 tree digest calculated from the ordered entries; and
- optional launchers, desktop entries, icons, and package policies.

Only directories, regular files, and relative symlinks are supported. Entry paths
must be UTF-8, relative, unique, strictly sorted, and safe to resolve inside the
package root. Paths cannot contain absolute components, `.` or `..`. Modes are
limited to `0o777`; verification compares only read and execute bits. Symlinks must
resolve lexically within the package tree. The manifest also rejects unknown fields,
unknown schemas, unsupported targets, missing integration sources, and malformed
package names.

The tree digest is domain-separated with `PAKO-TREE-V1` and includes the entry type,
path, normalized mode, and, for files, size and file digest. It detects additions,
deletions, type changes, content changes, and relevant permission changes.

### Payload archive

`payload.tar.zst` is a Zstandard-compressed tar archive of the complete package
tree. It is intentionally simple: every installation and upgrade downloads the
whole archive for its selected release. The local cache is used only for downloaded
temporary artifacts, not for reconstructed shared content.

During extraction, Pako rejects archive members other than files, directories, and
symlinks. It validates each archive path and symlink target before unpacking and
refuses to write through a symlink ancestor. Extraction occurs only in a private
staging directory.

## Trust and distribution

Repository configuration provides the trusted TUF root, metadata URL, targets URL,
and a local TUF datastore. `pako-trust` refreshes this metadata through `tough`,
then reads the signed `catalog.json` target.

The catalog has schema `1` and maps a package's channel (for example, `stable`) to a
release. A release contains its upstream version, Pako release number, target, OCI
reference, and expected OCI manifest digest. Resolution validates the package name,
channel, target, and catalog shape before any package installation starts.

The client resolves the signed catalog entry, fetches the OCI manifest by digest,
and verifies that digest. It verifies the image media type and requires exactly one
Pako package-manifest layer and one payload layer. The manifest blob is hashed and
parsed, and the payload descriptor must equal the payload descriptor inside the
package manifest. The payload transfer writes a `.partial` file, resumes with HTTP
Range when the registry supports it, hashes the completed file, and renames it into
place only on a matching digest.

The OCI client supports HTTPS by default, optional basic authentication for
publication, resumable pulls, blob existence checks, and OCI manifests/indexes.
Plain HTTP is an explicit local-development option only.

## Build and publication pipeline

Recipes are trusted maintainer-side inputs, not client-side package scripts. The
schema is documented in [RECIPE_FORMAT.md](RECIPE_FORMAT.md). A recipe may define
local or checksummed remote sources, archive extraction, transformations,
assertions, integrations, and target-specific build phases.

`pako-build` follows this pipeline:

1. Parse and validate `recipe.toml`; unknown fields are rejected.
2. Fetch or copy sources and verify their SHA-256 digests. Local paths must stay
   within the recipe directory.
3. Safely extract supported archives (`tar.gz`, `tar`, and `zip`) or place ordinary
   files at their declared destination.
4. Apply recipe transforms and assertions. If present, run build phases in the
   configured, digest-pinned environment; scripts are only trusted build-side code.
5. Walk the final payload tree without following links, reject unsupported entries,
   calculate file digests, and create a sorted manifest entry list.
6. Create `payload.tar.zst` deterministically from that tree, calculate its digest
   and size, calculate the tree digest, validate the manifest, and write canonical
   `package-manifest.json`.
7. On publish, revalidate the artifacts, upload the config, package manifest, and
   payload blobs, publish the platform manifest and image index, then add the
   immutable release to `catalog.json` and sign the local TUF repository.

`pako-build tuf init` creates a single-key development repository. Its generated
key is for local development and tests; production signing must use separate role
keys in a dedicated signing system.

## Installation and upgrade

Install and upgrade use the same pipeline after resolution. The only difference is
that upgrade takes its default channel from the existing package state.

```text
signed catalog resolution
  → OCI manifest and layer validation
  → verified payload download
  → package lock and transaction journal
  → safe extraction to staging
  → full entry and tree verification
  → integration preflight and private preparation
  → rename staged tree into the versioned cellar
  → atomically replace current symlink and publish integrations
  → save receipt/state, finalize integrations, remove journal
```

Before extraction, `pako-core` hashes the complete downloaded payload and compares
both its digest and byte count to the package manifest. The staged tree is verified
in parallel, up to the selected job count. Verification rejects undeclared paths and
checks every declared directory, file, and symlink; file verification includes mode,
size, and SHA-256 content digest. It then checks the manifest's tree digest.

The installer records immutable identifiers in each receipt: the repository, OCI
manifest digest, package-manifest digest, payload digest, and tree digest. That
makes the local installation traceable to exactly what was resolved and installed.

## Local layout and state

All state is private to the current user. With ordinary XDG defaults, Pako uses:

```text
$XDG_DATA_HOME/pako/
  cellar/<package>/<upstream-version>-<release>/  immutable installed trees
  apps/<package>/current                           active-version symlink
  manifests/<package>/<version>.json               stored release manifest
  staging/                                         same-filesystem transaction staging

$XDG_STATE_HOME/pako/
  packages/<package>.json                          active version, history, channel
  versions/<package>/<version>.json                immutable install receipt
  transactions/<id>.json                           durable recovery journal
  locks/                                           per-package and global exposure locks

$XDG_CACHE_HOME/pako/                              downloaded package artifacts and TUF data
$XDG_CONFIG_HOME/pako/                             repository configuration
~/.local/bin/                                      package launchers
$XDG_DATA_HOME/applications/                       desktop entry files
$XDG_DATA_HOME/icons/hicolor/                      package icons
```

`Layout::for_test` builds the same structure under a temporary root. Tests therefore
exercise the production path layout without touching a developer's XDG state.

## Activation, integration, and recovery

Each package has an exclusive file lock. Once a tree is verified, it is renamed from
staging to the versioned cellar on the same filesystem. Activation creates a
temporary symlink and atomically renames it over `apps/<package>/current`; installed
version directories remain immutable and are retained for rollback.

Integrations are handled under a separate global exposure lock because different
packages can otherwise race to claim the same launcher, desktop file, or icon.
Pako preflights all destinations before activation. It only replaces a file when it
matches a recorded receipt for the package being upgraded. New content and backup
copies use private sibling paths, are recorded in the journal, and are published or
restored idempotently. This prevents silently overwriting unmanaged files or another
package's integration.

The journal tracks `started`, `materialized`, `verified`, `treeCommitted`,
`committing`, and `complete` phases. Before the durable commit intent, recovery
removes incomplete staging state. Once the journal marks roll-forward intent,
recovery completes activation and integration publication using the recorded commit
plan. `pako recover` handles remaining journals explicitly, and package-mutating
commands recover outstanding work before they proceed.

## Lifecycle commands

- `install` resolves a package/channel from signed metadata and installs a new
  version; the same version cannot be installed twice.
- `upgrade` resolves the remembered channel (or an override) and uses the same
  verified transaction path; `--dry-run` stops after resolution and planning.
- `verify` performs a full integrity check of the active tree without changing it.
- `rollback` verifies a retained version before switching `current`; it does not
  download data.
- `versions` lists retained versions, and `prune --keep <count>` removes older,
  non-active versions after confirmation.
- `remove` removes package-owned integrations, installed versions, manifests, and
  state, while retaining shared cache data.
- `list` and `status` read receipts only. `recover` processes interrupted journals.

## Security boundaries and limitations

The signed catalog authenticates release selection; the OCI descriptor and blob
digests authenticate the transferred objects; the package manifest authenticates the
extracted file tree. These are separate checks, and all must succeed before a
release becomes active.

Pako deliberately does not sandbox a package at runtime, make arbitrary vendor
applications safe to execute, or protect application data stored outside the package
tree. `payloadMutation`, `selfUpdate`, and `userData` manifest policies currently
describe an external-management model; the client does not run package update code.

There is currently no content deduplication between releases, delta update format,
or automatic repair/download of damaged files. Reinstalling or upgrading obtains a
complete verified payload. Retained versions are useful for rollback but consume
their full installed size until pruned.
