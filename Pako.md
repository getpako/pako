# Pako 0.1.0 — Architecture and Implementation Specification

> **Document status:** target specification for the first public release, `0.1.0`.
>
> **Source of truth:** this document replaces the previous experimental pre-release architecture, including the single-archive and version-to-version delta design. The workspace in this repository is the reference implementation of this contract; any implementation/documentation mismatch is a bug and must be resolved before `0.1.0`.
>
> **No backward compatibility requirement:** Pako has not had a public release yet. The implementation must not preserve any experimental pre-release archive formats, legacy receipts, or the version-to-version delta-chain resolver. They may be removed before `0.1.0`.
>
> **Implementation contract for the coding agent:** when this document uses the words **must**, **must not**, **required**, or **forbidden**, the requirement is normative. Optional optimizations must never weaken correctness, integrity verification, or transactional behavior.

---

## 1. Project purpose

Pako is a user-space package manager for large, self-contained Linux applications. It is intended primarily for software distributed by vendors as archives rather than as native distribution packages, including:

- IntelliJ IDEA;
- Android Studio;
- PyCharm and other IDEs;
- Electron-based editors and desktop applications;
- database clients;
- developer tools and SDKs distributed as vendor archives.

Pako must provide:

1. installation into XDG-compliant user directories;
2. no `sudo` requirement;
3. small updates through reuse of unchanged content;
4. atomic activation of a newly installed version;
5. retention of the previous version for rollback;
6. detection of files modified outside Pako;
7. repair of damaged installations without redownloading the whole application;
8. package distribution through OCI, initially using GitHub Container Registry;
9. verification of repository metadata authenticity and integrity of every downloaded object.

Pako is **not** a replacement for `apt`, `dnf`, `pacman`, or another system package manager. It does not manage the kernel, drivers, `glibc`, system services, or files under `/usr`.

### 1.1. Project identity and GitHub organization

The public project identity is:

```text
Project:       Pako
CLI command:   pako
GitHub org:    https://github.com/getpako
OCI namespace: ghcr.io/getpako
```

Recommended repository layout for the first release:

```text
getpako/pako
    Main Rust workspace, CLI, client, builder, OCI support, trust layer,
    specifications, and release tooling.

getpako/packages
    Reviewed package recipes, patches, package metadata, and repository-specific
    Python automation that discovers upstream releases and opens update pull
    requests. That automation is not part of the Pako recipe format.

getpako/build-images
    Optional trusted build environments for source packages. This repository may
    be created when the first source-built packages are introduced.
```

Only `getpako/pako` and `getpako/packages` are required to start development.
The architecture must not depend on a website or additional service repository.

---

## 2. Core architectural decision

Pako `0.1.0` uses one distribution model:

```text
pako-chunked-v1
```

There are no separate concepts of:

```text
full package
separate delta 1.0 -> 1.1
separate delta 1.1 -> 1.2
```

Every application release is represented by a manifest of the expected file tree. Regular files are described as ordered lists of content-addressed chunks.

```text
application release
    -> package manifest
    -> ordered SHA-256 chunk lists
    -> chunks stored in immutable packfiles
    -> packfiles published as OCI blobs
```

A first installation and an upgrade run the same algorithm:

```text
resolve required chunks
    -> find chunks already available locally
    -> download packs containing missing chunks
    -> materialize the new file tree
    -> verify the result
    -> atomically activate the version
```

The only difference is how much content is already available locally.

A "full installation" therefore means that no required chunks are available. A "delta update" means that most required chunks are already present and only missing content is downloaded. Both operations use the same package format and the same installer path.

---

## 3. Frozen decisions for `0.1.0`

The following decisions define format version `v1`. They must not be changed without introducing a new format version.

| Area | Decision |
| --- | --- |
| Operating system | Linux only |
| Architectures | `x86_64`, `aarch64` |
| Transport | OCI Distribution API |
| Initial hosting | GitHub Container Registry |
| OCI, file, tree, pack, and chunk digests | SHA-256 |
| Content-defined chunking | FastCDC v2020 |
| Chunking profile | `pako-fastcdc-v1` |
| Small files | one whole-file chunk below 256 KiB |
| FastCDC minimum | 256 KiB |
| FastCDC average | 1 MiB |
| FastCDC maximum | 4 MiB |
| Chunk compression | independent Zstandard frames |
| Preferred maximum pack size | 16 MiB of stored data |
| Hard pack size limit | 32 MiB |
| Upgrade mechanism | download missing packs/chunks |
| Activation | atomic replacement of the `current` symlink |
| Installation layout | versioned, immutable directories |
| Previous version | retained by default for rollback |
| Repository trust | signed, expiring metadata based on TUF |
| Recipe schema | numeric `schema = 1` in `recipe.toml` |
| Prebuilt recipes | target-specific archives, checksums, and extraction rules |
| Source recipes | sandboxed build phases with trusted build-side shell scripts |
| Client-side package scripts | forbidden |
| Release discovery automation | outside the Pako format and maintained in `getpako/packages` |

KiB and MiB use powers of 1024.

The exact FastCDC size profile must be benchmarked before the format is permanently frozen, but the architecture, manifest model, and content-addressed storage design are already final for `0.1.0`.

---

## 4. Non-functional requirements

### 4.1. Security

Pako must not:

- write outside its XDG directories and explicitly declared user integration directories;
- execute scripts downloaded from a package during client-side installation;
- trust a filename, mutable OCI tag, URL, or unsigned index as an identity;
- extract absolute paths, `..` traversal, devices, sockets, or FIFOs;
- follow symlinks while materializing regular files;
- activate a version whose resulting digests do not match the manifest;
- overwrite an exposure owned by another package or an unmanaged user file;
- use unverified cached content in a final installation;
- silently accept an expired or rolled-back repository metadata set.

### 4.2. Crash safety

After a crash, process termination, or power loss, exactly one of these states must be recoverable:

- the previous version remains complete and active;
- the new version is complete and active.

An intermediate state must be detected and repaired automatically from the transaction journal.

### 4.3. Bounded memory use

No application file, packfile, or complete package may be required entirely in memory. Chunking, downloading, verification, decompression, and materialization must be streaming operations.

### 4.4. Packaging determinism

Given the same final payload tree, chunking profile, packer version, and canonical
serialization rules, Pako must produce identical:

- chunk boundaries;
- chunk digests;
- package manifests;
- packfiles;
- pack indexes;
- OCI manifests.

A source build may contain upstream toolchains that are not bit-for-bit
reproducible. Running every source build twice and comparing outputs is not a
`0.1.0` requirement. The publisher must still record the recipe revision, source
digests, build-environment digest, and final tree digest in the build report.

### 4.5. Backend portability

The client must not depend on GitHub-specific behavior in its domain layer. GHCR is the first OCI registry deployment, not part of the package model.

### 4.6. Correctness before optimization

Optional capabilities such as HTTP Range, local chunk recovery, pack reuse heuristics, or metadata caching may improve performance. Their absence or failure must never make installation incorrect. The client must always be able to fall back to downloading complete required packs and verifying all resulting content.

---

## 5. Terminology

### Package

A logical application, for example `intellij-idea`.

### Package release

A concrete combination of:

```text
name + upstream version + Pako release + target
```

Example:

```text
intellij-idea 2026.1.1 release 1 linux/x86_64
```

The Pako `release` number is incremented when the recipe or packaging changes without an upstream version change.

### Target

A supported platform tuple:

```text
linux/x86_64
linux/aarch64
```

### Chunk

A sequence of raw bytes from one regular file. Its identity is the SHA-256 digest of the uncompressed bytes.

### Packfile

An immutable file containing multiple independently compressed chunks plus a deterministic index.

### Package manifest

A deterministic description of the expected file tree, file metadata, desktop integrations, and ordered chunk lists.

### Pack index

A mapping:

```text
chunk digest -> pack digest + offset + stored size + raw size
```

### Receipt

An atomically written local record describing an installed and active package release.

### Materialization

Reconstruction of a complete version directory from verified chunks.

### Exposure

A file or link created outside the package version directory, such as a launcher under `~/.local/bin`, a `.desktop` entry, or an icon.

### Object store

The local content-addressed cache for chunks and packs, keyed by SHA-256.

### Dirty installation

An installed tree that no longer matches the package manifest because files were changed, removed, or added outside Pako.

---

## 6. System invariants

The implementation must preserve these invariants:

1. Published chunks, packs, package manifests, pack indexes, and OCI manifests are immutable.
2. Data identity is a digest, never a mutable tag or URL.
3. A tag such as `stable` is used only for discovery; the installed receipt records exact digests.
4. Every application version lives in a separate directory.
5. The active version is selected through a `current` symlink.
6. Pako never updates the active version directory in place.
7. Staging is located on the same filesystem as the final version directory so the final rename is atomic.
8. Every chunk read from cache or an existing installation is verified before use.
9. Every reconstructed file digest and the final tree digest are verified before activation.
10. A package never executes code during client-side installation.
11. A manifest path may never escape the installation root.
12. A package symlink must be relative and resolve inside the installation root after lexical normalization.
13. Device files, sockets, FIFOs, and hard links are not supported in the package manifest.
14. A package may not take ownership of an exposure owned by another package.
15. If activation fails, the previous symlink, receipt, and exposures are restored.
16. The previously active version is not automatically deleted after upgrade.
17. Deleting the cache must not damage installed applications.
18. The client does not trust mtime or file size as proof of integrity; they may only be used as a verification optimization.
19. Repository metadata must protect against replacement, rollback, mix-and-match, and freeze attacks.
20. A missing optimization must not reduce correctness; full required packs remain a valid fallback.
21. The client never trusts decompressed length, offset, count, or allocation values before enforcing explicit limits.
22. No temporary file becomes visible at its final path until its digest has been verified.
23. Receipts and transaction records are written atomically and fsynced before dependent state transitions.
24. Package selection is deterministic for the same trusted repository snapshot and target.

---

## 7. Target Rust workspace

The codebase should become a Rust workspace. Names may be adjusted slightly, but responsibilities must remain separated.

```text
crates/
├── pako-core/
├── pako-cli/
├── pako-build/
├── pako-oci/
├── pako-trust/
└── pako-test-support/
```

### `pako-core`

Registry-independent and CLI-independent domain code:

- manifest models;
- canonical serialization;
- package name, version, target, and path validation;
- FastCDC integration;
- SHA-256 and tree digests;
- packfile format;
- pack index;
- local chunk sources;
- download planning;
- materialization;
- receipts;
- installation transactions;
- XDG layout;
- verify, repair, rollback, cleanup, and ownership logic.

### `pako-cli`

- argument parsing;
- human-readable progress and diagnostics;
- JSON output;
- mapping domain errors to exit codes;
- dependency wiring across crates;
- confirmation prompts for destructive actions.

### `pako-build`

A separate publishing tool maintained in the same repository but not required by normal users:

- recipe parsing and validation;
- target-specific source download and signature/checksum verification;
- safe preparation of pinned archive and file sources;
- isolated execution of source build phases;
- output tree normalization and audit;
- chunking;
- pack construction;
- lookup of already published chunks;
- OCI artifact generation;
- publishing;
- canonical catalog target generation for an external TUF signing pipeline;
- build reports and release statistics;
- strict separation between untrusted recipe execution and publishing credentials.

`pako-build` must support both prebuilt and source recipes. Source recipe scripts
run only inside a restricted sandbox. Normal clients do not need this crate or
its build environments.

### `pako-oci`

- OCI Distribution API implementation;
- resolving a manifest by tag or digest;
- pulling manifests and blobs;
- resumable blob downloads;
- blob upload and existence checks;
- publishing OCI indexes and manifests;
- registry authentication;
- no installation-domain logic.

The client must not require `docker`, `podman`, `oras`, or `skopeo`.

A registry abstraction should be close to:

```rust
#[async_trait::async_trait]
pub trait Registry: Send + Sync {
    async fn resolve_manifest(
        &self,
        reference: &OciReference,
    ) -> Result<Descriptor>;

    async fn fetch_manifest(
        &self,
        digest: &Sha256Digest,
    ) -> Result<Vec<u8>>;

    async fn fetch_blob(
        &self,
        digest: &Sha256Digest,
        destination: &Path,
    ) -> Result<()>;

    async fn push_blob(
        &self,
        source: &Path,
    ) -> Result<Sha256Digest>;

    async fn push_manifest(
        &self,
        reference: &OciReference,
        bytes: &[u8],
    ) -> Result<Sha256Digest>;
}
```

### `pako-trust`

- trusted metadata refresh;
- signature verification;
- expiration checks;
- version monotonicity checks;
- rollback and freeze protection;
- mapping a package release and target to an exact OCI manifest digest.

Do not design a custom cryptographic protocol. Use a reviewed TUF implementation or a compatible, reviewed implementation of the TUF security model.

### `pako-test-support`

- isolated temporary XDG layouts;
- deterministic sample tree generation;
- local mock OCI registry;
- fault injection;
- helpers for transaction assertions;
- test signing keys and repository snapshots.

---

## 8. User directory layout

Default paths:

```text
$XDG_DATA_HOME/pako/
├── cellar/<name>/<upstream-version>-<release>/
├── apps/<name>/current
├── manifests/sha256/<prefix>/<digest>.json
└── staging/<transaction-id>/

$XDG_STATE_HOME/pako/
├── receipts/<name>.json
├── transactions/<transaction-id>.json
├── locks/
├── ownership.json
└── verification/<name>.json

$XDG_CACHE_HOME/pako/
├── packs/sha256/<prefix>/<digest>
├── chunks/sha256/<prefix>/<digest>
├── repository/<repo-name>/
├── downloads/
└── partial/

$XDG_CONFIG_HOME/pako/
└── repositories.toml

$HOME/.local/bin/
└── <launcher>

$XDG_DATA_HOME/applications/
└── pako-<package>-<entry>.desktop

$XDG_DATA_HOME/icons/hicolor/<size>/apps/
└── pako-<package>-<icon>.<ext>
```

When an XDG variable is unset, use the standard fallback:

```text
XDG_DATA_HOME   = ~/.local/share
XDG_STATE_HOME  = ~/.local/state
XDG_CACHE_HOME  = ~/.cache
XDG_CONFIG_HOME = ~/.config
```

`PAKO_BIN_HOME` may explicitly override the launcher directory.

All base paths must be:

- converted to absolute paths;
- lexically normalized;
- checked before each state-changing operation;
- rejected if an expected directory is a regular file or unsafe symlink.

Pako should refuse to run installation operations as `root` unless an explicit development-only override is supplied. Running as root is not Pako's system installation mode.

---

## 9. Package names, versions, releases, and targets

### Package name

Allowed format:

```regex
^[a-z0-9][a-z0-9._-]{0,127}$
```

A name must not contain path separators, must not equal `.` or `..`, and must not end with a dot.

### Upstream version

Allowed format:

```regex
^[A-Za-z0-9][A-Za-z0-9._+~-]{0,127}$
```

The upstream version is display and ordering metadata. It must not be interpolated into filesystem paths before validation.

### Pako release

A positive integer beginning at `1`:

```text
2026.1.1-1
2026.1.1-2
```

The release number changes when packaging changes while the upstream application version remains the same.

### Target

Canonical target values:

```text
linux/x86_64
linux/aarch64
```

Rust architecture aliases and upstream naming must be normalized before package resolution:

```text
amd64  -> x86_64
x64    -> x86_64
arm64  -> aarch64
```

A release must never mix files for multiple architectures in one package manifest.

### Release identity

The canonical logical identifier is:

```text
<name>@<upstream-version>-<release>#<target>
```

Example:

```text
intellij-idea@2026.1.1-1#linux/x86_64
```

Filesystem directory names must be derived only from validated fields.

---

## 10. Repository and trust model

### 10.1. Repository configuration

`repositories.toml` example:

```toml
schema_version = 1

[[repositories]]
name = "core"
registry = "ghcr.io"
namespace = "getpako/core"
enabled = true
priority = 100
trust_root = "~/.config/pako/trust/core-root.json"
```

Repository names follow the package-name character restrictions. Priorities are deterministic: higher priority wins, with repository name as a stable tie-breaker.

### 10.2. Trust roles

The trusted metadata model must provide the equivalent of these TUF roles:

```text
root
snapshot
timestamp
targets
```

Required properties:

- an offline root of trust;
- threshold signatures where appropriate;
- expiring online metadata;
- monotonically increasing metadata versions;
- protection against rollback and freeze attacks;
- snapshot consistency across package targets;
- exact digest and size binding for target OCI manifests.

The root metadata is bootstrapped explicitly when a repository is added. Subsequent root rotation must follow TUF root update rules.

### 10.3. Package target catalog

Trusted targets metadata maps a package release and target to an exact OCI descriptor.

Conceptual entry:

```json
{
  "path": "packages/intellij-idea/2026.1.1-1/linux/x86_64",
  "length": 2143,
  "hashes": {
    "sha256": "..."
  },
  "custom": {
    "package": "intellij-idea",
    "upstreamVersion": "2026.1.1",
    "release": 1,
    "target": "linux/x86_64",
    "metadata": {
      "displayName": "IntelliJ IDEA",
      "summary": "Integrated development environment for Java and Kotlin",
      "vendor": "JetBrains",
      "homepage": "https://www.jetbrains.com/idea/",
      "license": "LicenseRef-JetBrains-IntelliJ-IDEA"
    },
    "channel": "stable",
    "ociReference": "ghcr.io/getpako/core/intellij-idea",
    "ociManifestDigest": "sha256:..."
  }
}
```

The client must treat the signed `ociManifestDigest` as authoritative. It must not install a different manifest merely because a tag points to it.

### 10.4. Adding a repository

Expected command:

```bash
pako repo add core \
  oci://ghcr.io/getpako/core \
  --root ./core-root.json
```

The operation must:

1. validate the repository name and URI;
2. read and validate root metadata;
3. show root key fingerprints and threshold information;
4. require confirmation unless `--yes` is supplied;
5. copy the root metadata atomically into configuration storage;
6. refresh timestamp, snapshot, and targets metadata;
7. persist the repository only after successful validation.

A repository must never be silently trusted from a remote URL alone.

---

## 11. OCI artifact structure

### 11.1. Naming

Recommended package repository:

```text
ghcr.io/getpako/core/intellij-idea
```

Discovery tags may include:

```text
2026.1.1-1
stable
beta
```

The client resolves a tag only through signed repository metadata and stores the exact OCI digest in its receipt.

### 11.2. Multi-architecture index

A package version tag may resolve to an OCI image index containing platform-specific manifests:

```text
OCI index
├── linux/x86_64 platform manifest
└── linux/aarch64 platform manifest
```

OCI platform fields should use canonical OCI names:

```text
linux/amd64
linux/arm64
```

The Pako package manifest still records Pako's normalized target:

```text
linux/x86_64
linux/aarch64
```

### 11.3. Platform manifest

Recommended OCI manifest:

```json
{
  "schemaVersion": 2,
  "mediaType": "application/vnd.oci.image.manifest.v1+json",
  "artifactType": "application/vnd.pako.package.v1",
  "config": {
    "mediaType": "application/vnd.pako.package.config.v1+json",
    "digest": "sha256:...",
    "size": 512
  },
  "layers": [
    {
      "mediaType": "application/vnd.pako.package-manifest.v1+json",
      "digest": "sha256:...",
      "size": 183421
    },
    {
      "mediaType": "application/vnd.pako.pack-index.v1+json",
      "digest": "sha256:...",
      "size": 924151
    },
    {
      "mediaType": "application/vnd.pako.chunk-pack.v1",
      "digest": "sha256:...",
      "size": 15871234
    }
  ],
  "annotations": {
    "org.opencontainers.image.title": "intellij-idea",
    "org.opencontainers.image.version": "2026.1.1",
    "org.opencontainers.image.source": "https://github.com/getpako/packages",
    "dev.pako.release": "1",
    "dev.pako.target": "linux/x86_64"
  }
}
```

The package manifest and pack index are first-class blobs. Pack layers contain only immutable chunk data.

A release may reference pack blobs already used by older releases. OCI blob identity makes that reuse natural and avoids re-uploading identical packfiles.

### 11.4. Publishing rule

Publishing order must prevent a partially visible release:

1. upload all new pack blobs;
2. upload package manifest and pack index blobs;
3. upload the platform manifest by digest;
4. upload or update the multi-platform index;
5. publish signed targets metadata that references the exact OCI digest;
6. publish snapshot metadata;
7. publish timestamp metadata last.

A release is considered available only after the new signed timestamp metadata is published.

---

## 12. Package manifest: `pako.package-manifest.v1`

The package manifest describes the result that must exist on disk. It does not describe where chunks are stored.

Canonical JSON example:

```json
{
  "schemaVersion": 1,
  "mediaType": "application/vnd.pako.package-manifest.v1+json",
  "package": "intellij-idea",
  "upstreamVersion": "2026.1.1",
  "release": 1,
  "target": "linux/x86_64",
  "metadata": {
    "displayName": "IntelliJ IDEA",
    "summary": "Integrated development environment for Java and Kotlin",
    "description": "IntelliJ IDEA is an integrated development environment for JVM technologies.",
    "vendor": "JetBrains",
    "homepage": "https://www.jetbrains.com/idea/",
    "license": "LicenseRef-JetBrains-IntelliJ-IDEA"
  },
  "chunking": {
    "profile": "pako-fastcdc-v1",
    "algorithm": "fastcdc-v2020",
    "smallFileThreshold": 262144,
    "minimum": 262144,
    "average": 1048576,
    "maximum": 4194304
  },
  "treeDigest": "sha256:...",
  "entries": [],
  "integrations": {
    "launchers": [],
    "desktopEntries": [],
    "icons": []
  },
  "policies": {
    "payloadMutation": "deny",
    "selfUpdate": "external",
    "userData": "external"
  }
}
```

Serialization must be canonical and deterministic. Unknown required fields must be rejected. Unknown optional fields may only be accepted when the schema explicitly marks them as forward-compatible.

### 12.1. Directory entry

```json
{
  "type": "directory",
  "path": "lib",
  "mode": 493
}
```

`mode` contains only permission bits relevant to user-space materialization. Ownership, setuid, setgid, and sticky bits are forbidden.

### 12.2. Regular-file entry

```json
{
  "type": "file",
  "path": "lib/platform.jar",
  "mode": 420,
  "size": 182734821,
  "digest": "sha256:...",
  "chunks": [
    {
      "digest": "sha256:...",
      "size": 983221
    },
    {
      "digest": "sha256:...",
      "size": 1203811
    }
  ]
}
```

Required validation:

- chunk sizes sum exactly to `size`;
- zero-length files contain an empty chunk list and use the SHA-256 digest of empty content;
- no chunk exceeds the profile maximum except a whole-file chunk allowed by an explicitly versioned rule;
- the file digest is computed over raw bytes in chunk order;
- executable permission is represented only by mode bits;
- sparse files are materialized as ordinary files in `0.1.0`.

### 12.3. Symlink entry

```json
{
  "type": "symlink",
  "path": "bin/idea",
  "target": "idea.sh"
}
```

Requirements:

- `target` must be relative;
- no NUL byte;
- lexical resolution from the symlink's parent must stay inside the package root;
- symlinks are created after all directories and regular files;
- materialization must not follow an existing symlink at the destination.

### 12.4. Ordering and uniqueness

Entries must be sorted by raw UTF-8 path bytes after normalization.

The manifest must reject:

- duplicate paths;
- a file that is also an ancestor of another entry;
- path aliases caused by repeated separators or `.` components;
- invalid UTF-8 in `v1`;
- case folding is not performed on Linux, but exact byte uniqueness is required.

### 12.5. Integrations

Integrations refer only to paths already declared in the package tree.

#### Launcher

```json
{
  "name": "intellij-idea",
  "target": "bin/idea",
  "arguments": []
}
```

The generated launcher must resolve the active version at runtime through `apps/<package>/current`. It must not embed a specific version directory.

#### Desktop entry

```json
{
  "id": "intellij-idea",
  "name": "IntelliJ IDEA",
  "exec": "intellij-idea %f",
  "icon": "pako-intellij-idea",
  "terminal": false,
  "categories": ["Development", "IDE"]
}
```

The publisher provides structured fields, not arbitrary desktop file text. The client renders a deterministic `.desktop` file and escapes values according to the Desktop Entry specification.

#### Icon

```json
{
  "name": "pako-intellij-idea",
  "source": "bin/idea.svg",
  "context": "apps",
  "size": "scalable"
}
```

The icon source must exist in the package tree and match an allowed image type.

### 12.6. Self-update policy

Allowed values:

```text
selfUpdate = external
selfUpdate = supported
selfUpdate = unknown
```

For vendor applications managed by Pako, the normal value is:

```text
payloadMutation = deny
selfUpdate = external
```

This means Pako owns application binaries and vendor self-updates must be disabled or ignored. User configuration, plugins, caches, and projects remain outside the version directory.

The policy is informative and may drive launcher configuration, but correctness must still rely on digest verification rather than trusting the application to comply.


---

## 13. Tree digest

The tree digest provides one canonical identity for the complete materialized package tree. It is calculated from a deterministic byte stream and then hashed with SHA-256.

The stream begins with:

```text
16 bytes magic = "PAKO-TREE-V1\0\0\0\0"
```

Then, for every manifest entry sorted by normalized path:

### Directory

```text
u8      type = 1
u32_le  path_length
bytes   UTF-8 path
u16_le  normalized_mode
```

### Regular file

```text
u8       type = 2
u32_le   path_length
bytes    UTF-8 path
u16_le   normalized_mode
u64_le   size
32 bytes raw file SHA-256
```

### Symlink

```text
u8      type = 3
u32_le  path_length
bytes   UTF-8 path
u32_le  target_length
bytes   UTF-8 target
```

`normalized_mode` may contain only read and executable permission bits represented by `0444` and `0111`. Write bits, ownership, group, timestamps, inode values, and filesystem allocation details do not affect the tree digest.

Installation verification must:

1. scan the real tree without following symlinks;
2. reject every undeclared entry;
3. compute SHA-256 for every regular file;
4. build the canonical stream above;
5. compare its SHA-256 with `treeDigest`.

The tree digest is not a substitute for per-file and per-chunk verification. It is the final aggregate check.

---

## 14. Content-defined chunking

### 14.1. Profile `pako-fastcdc-v1`

```text
algorithm:              FastCDC v2020
small-file threshold:   256 KiB
minimum chunk:          256 KiB
average chunk:            1 MiB
maximum chunk:            4 MiB
```

Rules:

- a file smaller than 256 KiB is represented as one whole-file chunk;
- a file equal to or larger than 256 KiB is split using FastCDC;
- an empty file has no chunks;
- boundaries are calculated from raw file bytes after recipe normalization;
- the compressed archive supplied by the vendor is not chunked as one stream;
- internal formats such as JAR, ZIP, AppImage, or database files are not unpacked only to improve chunk reuse;
- a chunk never crosses a regular-file boundary.

### 14.2. Code abstraction

FastCDC must be hidden behind a stable domain abstraction:

```rust
pub trait Chunker {
    fn profile(&self) -> ChunkingProfile;

    fn chunk<R: std::io::Read + Send + 'static>(
        &self,
        reader: R,
    ) -> Result<Box<dyn Iterator<Item = Result<ChunkBoundary>> + Send>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkBoundary {
    pub offset: u64,
    pub length: u32,
}
```

The concrete library API must not leak into package manifests or the rest of the installer. The profile name and all relevant parameters are persisted so a future format can coexist with `v1`.

### 14.3. Hashing

FastCDC's Gear hash is used only to select boundaries. It is not an integrity digest.

For each chunk, the publisher must calculate SHA-256 over raw, uncompressed bytes. File SHA-256 must be calculated over the complete file stream independently of chunk boundaries.

### 14.4. Determinism requirements

Tests must prove that:

- the same file always produces the same boundaries;
- boundaries are independent of the `Read` buffer size;
- a dependency update cannot change boundaries without failing compatibility vectors;
- inserting bytes near the beginning of a file resynchronizes after a limited region;
- build concurrency and file traversal order do not change results.

The exact FastCDC crate version and algorithm variant must be pinned in `Cargo.lock`. Changing the variant or boundary behavior requires a new chunking profile.

### 14.5. Benchmark before format freeze

Before publishing `0.1.0`, run the same test corpus with average chunk sizes of 512 KiB, 1 MiB, and 2 MiB. At minimum, include consecutive releases of:

- IntelliJ IDEA;
- Android Studio;
- another JetBrains IDE;
- Visual Studio Code or another Electron application;
- one large SDK.

Record:

- total chunk count;
- unique new bytes per upgrade;
- compressed download size;
- manifest and pack-index size;
- publisher CPU time;
- client reconstruction time;
- peak memory;
- overfetch caused by whole-pack downloads.

The selected profile must be documented by a committed benchmark report and compatibility vectors.

---

## 15. Packfile format: `PAKPACK1`

A packfile is immutable and identified by SHA-256 of its complete stored bytes.

### 15.1. Design requirements

- one pack contains multiple chunks;
- every chunk is independently compressed;
- a chunk can be read and verified without decompressing other chunks;
- a pack is not a tar archive;
- a pack contains no application file paths;
- chunk order is deterministic;
- a pack is not padded to 16 MiB;
- a small release may produce a pack smaller than 1 MiB;
- the format is seekable and already contains offsets required for future HTTP Range support.

### 15.2. Binary layout

All integer values are unsigned little-endian.

#### Header

```text
8 bytes  magic = "PAKPACK1"
u16      format_version = 1
u16      flags = 0
u32      entry_count
u64      index_offset
u64      index_length
32 bytes reserved = zero
```

The data region follows the header and contains independent chunk payloads.

#### Index entry

The index is stored at the end of the file. Entries are sorted by the 32 raw bytes of the chunk SHA-256 digest.

```text
32 bytes raw_chunk_sha256
u64      data_offset
u64      stored_size
u64      raw_size
u8       compression
7 bytes  reserved = zero
```

Compression values:

```text
0 = raw
1 = independent zstd frame
```

The `v1` publisher uses Zstandard unless compression increases the stored size, in which case it may use `raw`.

The format must define and enforce one fixed Zstandard configuration for deterministic package output, including compression level and whether checksums or content sizes are written into frames.

### 15.3. Pack validation

The parser must reject:

- an invalid magic value or version;
- unknown non-zero flags;
- non-zero reserved bytes;
- arithmetic overflow while calculating ranges;
- an index or payload range outside the file;
- payload ranges overlapping the header or index;
- overlapping payloads;
- duplicate chunk digests;
- an unsorted index;
- a `raw_size` above the profile limit;
- an unknown compression type;
- a Zstandard frame producing more bytes than declared;
- trailing bytes not described by the format.

After extracting a chunk, the client must always:

1. enforce the exact declared `raw_size`;
2. calculate SHA-256 over raw output;
3. compare it with the expected chunk digest.

The OCI digest of the complete pack does not replace chunk verification.

### 15.4. Pack size policy

The publisher groups only newly introduced chunks into packs with a soft target of 16 MiB stored data.

Deterministic algorithm:

1. sort new chunks by digest;
2. compress each chunk independently using the fixed configuration;
3. append chunks to the current pack in sorted order;
4. if adding the next chunk would exceed the 16 MiB soft target, close the current pack;
5. the final pack may be arbitrarily smaller;
6. reject any pack above the 32 MiB hard limit;
7. one chunk larger than the hard pack limit is a format or profile error and must not be silently split differently.

Do not add old, unchanged chunks merely to fill a new pack.

---

## 16. Pack index: `pako.pack-index.v1`

The pack index is specific to one package release and target. It locates every chunk required by the package manifest.

```json
{
  "schema": "pako.pack-index.v1",
  "packageManifestDigest": "sha256:...",
  "packs": {
    "sha256:pack-a": {
      "size": 15728640
    },
    "sha256:pack-b": {
      "size": 8388608
    }
  },
  "chunks": {
    "sha256:chunk-a": {
      "pack": "sha256:pack-a",
      "offset": 64,
      "storedSize": 712381,
      "rawSize": 983221,
      "compression": "zstd"
    }
  }
}
```

Requirements:

- every chunk required by the package manifest appears exactly once;
- every referenced pack appears as a layer in the same OCI platform manifest;
- `rawSize` matches the package manifest;
- offset, size, digest, and compression match the internal `PAKPACK1` index;
- no range exceeds the pack descriptor size;
- all digests use canonical lowercase `sha256:<64 hex>` form;
- the pack index is deterministically sorted during serialization;
- `packageManifestDigest` binds the index to one exact package manifest.

The pack index stores offsets so future clients can use HTTP Range. Correctness in `0.1.0` must not depend on Range support; the client downloads the complete pack blob.

---

## 17. Reusing packs across releases

Packs are immutable.

Example:

```text
release 1 requires: A B C D
pack-r1-1 contains: A B C D

release 2 requires: A B X D
```

The publisher does not rebuild `pack-r1-1` as `A B X D`. It publishes:

```text
pack-r2-1 contains: X
```

The release-2 OCI manifest references:

```text
pack-r1-1
pack-r2-1
```

The release-2 pack index maps:

```text
A -> pack-r1-1
B -> pack-r1-1
X -> pack-r2-1
D -> pack-r1-1
```

It is valid for an old pack to also contain an unused chunk such as `C`. The client downloads a pack only when it lacks at least one required chunk located in that pack.

### Build-side catalog

The publisher must maintain or reconstruct this mapping:

```text
chunk digest -> existing pack descriptor
```

It may reconstruct the mapping from all supported release manifests and indexes. This catalog is publishing state, not a separate client trust root.

For `0.1.0`, remote pack reuse is scoped to:

```text
package + operating system + architecture
```

Do not optimize remote deduplication across unrelated packages in the first release. The local chunk cache may naturally recognize identical raw chunks across packages because identity is global SHA-256.

### Retention rule

A pack may be deleted from the registry only when no supported, signed release references any required chunk stored exclusively in that pack. Registry cleanup must therefore be reachability-based, not age-based.

---

## 18. Local chunk sources

The planner may obtain a required chunk from these sources, in priority order:

1. verified raw chunk cache;
2. an installed active or retained package version;
3. a cached complete pack;
4. a pack downloaded from the registry.

### 18.1. Chunk cache

```text
$XDG_CACHE_HOME/pako/chunks/sha256/ab/cdef...
```

Atomic write procedure:

1. create a randomly named temporary file in the destination directory;
2. stream raw bytes into it;
3. enforce maximum size;
4. flush and fsync according to the durability policy;
5. calculate and verify SHA-256;
6. atomically rename it to the digest-derived path;
7. fsync the parent directory where required.

An existing digest path must never be overwritten with different bytes. A digest mismatch means corruption and the object must be quarantined or removed.

The chunk cache is an optimization and may be deleted without damaging an installation.

### 18.2. Recovery from an installed version

An installed version's manifest gives the ordered chunk sizes of every file. Pako can construct:

```text
chunk digest -> version root + file path + offset + length
```

Before using such a source:

1. open the file without following an unexpected symlink;
2. verify that the opened path belongs to the approved version directory;
3. read exactly the expected byte range;
4. calculate SHA-256;
5. use the bytes only when the digest matches.

It is unnecessary to trust the entire file or mtime. If an application's built-in updater changed one part of a file, unchanged expected chunks may still be reused and changed chunks will be rejected.

For the initial implementation, recovery may use old manifest boundaries directly. Re-running FastCDC over dirty files is optional and must not be required for correctness.

### 18.3. Pack cache

```text
$XDG_CACHE_HOME/pako/packs/sha256/ab/cdef...
```

Pack downloading must:

- support resuming a complete OCI blob where the registry supports it;
- write to a `.partial` file;
- bind partial state to expected digest and size;
- verify complete-pack SHA-256 against the OCI descriptor;
- validate the `PAKPACK1` structure before exposing the cache entry;
- atomically publish the verified pack;
- permit later cleanup through an LRU or explicit cleanup policy.

A cached pack still requires per-chunk verification during extraction.

---

## 19. Download planning

### 19.1. Inputs

The planner receives:

- the verified package manifest;
- the verified pack index;
- the raw chunk cache inventory;
- installed-version manifests and safe source paths;
- the complete-pack cache inventory.

### 19.2. `0.1.0` algorithm

```text
required_chunks = all unique chunks in the package manifest
local_chunks = verified cached chunks + candidate installed-file sources
missing_chunks = required_chunks - usable local_chunks
required_packs = unique packs containing missing_chunks
bytes_to_download = sizes of required_packs absent from pack cache
```

Each chunk has exactly one remote location in `v1`, so the planner does not need set-cover optimization or a version-to-version shortest-path algorithm.

The planner must report:

```text
total required chunks
chunks available in the raw cache
chunks recoverable from installed versions
missing chunks
packs already cached
packs to download
bytes to download
total stored size of all release packs
estimated reused raw bytes
installed size
```

### 19.3. Dry run

```bash
pako upgrade intellij-idea --dry-run
```

A dry run must not change installed state. It may refresh signed repository metadata and fetch small manifests or indexes, but it must not download large pack blobs unless an explicit option requests it.

### 19.4. Overfetch accounting

Because `0.1.0` downloads whole packs, the plan must distinguish:

```text
raw bytes required from downloaded packs
stored bytes downloaded
pack overfetch
```

This metric is important for tuning the pack-size policy and evaluating future Range support.

---

## 20. First installation

Command:

```bash
pako install intellij-idea
```

### 20.1. Resolve phase

1. acquire the global state lock or package-specific write lock;
2. recover unfinished transactions;
3. refresh and verify repository metadata;
4. select the repository according to priority and explicit user constraints;
5. resolve a channel or requested version;
6. select the host target;
7. obtain the exact signed OCI manifest digest;
8. fetch and verify the OCI manifest;
9. fetch and verify the package manifest and pack index;
10. validate every schema and all cross-references before downloading packs.

### 20.2. Plan phase

1. enumerate unique required chunks;
2. inspect the verified raw chunk cache;
3. inspect the pack cache;
4. calculate required packs and total download size;
5. show the user the selected version, source repository, installed size, and download size;
6. require confirmation unless `--yes` or non-interactive policy permits the action.

For a first installation, no installed version exists, but chunks already present from another operation may still be reused.

### 20.3. Fetch phase

1. create a transaction record;
2. download missing packs to partial paths;
3. verify OCI digest, size, and pack structure;
4. extract required chunks from verified packs;
5. verify each raw chunk digest;
6. publish chunks atomically into the object cache where caching policy allows;
7. update transaction progress after durable milestones.

### 20.4. Materialize phase

1. create a staging directory on the same filesystem as the final `cellar` directory;
2. create directories in manifest order;
3. create every regular file with restrictive temporary permissions;
4. stream its chunks in manifest order;
5. verify final byte count and file SHA-256;
6. apply normalized final permissions;
7. create symlinks only after regular files and directories;
8. scan and verify the complete tree digest;
9. mark payload directories read-only according to policy;
10. fsync required files and directories before commit.

### 20.5. Activate phase

1. atomically rename verified staging to the final version directory;
2. prepare launcher, desktop entry, and icon changes in temporary files;
3. atomically publish exposures while recording ownership;
4. atomically write the receipt;
5. create `current.new` pointing to the new version;
6. atomically rename `current.new` over `current`;
7. update desktop databases where available;
8. mark the transaction committed;
9. remove the transaction journal only after durable completion.

The precise ordering of receipt, exposures, and `current` must be coordinated with the transaction journal so recovery can always identify the authoritative active version and restore the previous state.

### 20.6. Result

Expected user output:

```text
Installed intellij-idea 2026.1.1-1
Downloaded: 2.74 GiB
Reused: 0 B
Installed size: 3.96 GiB
Launcher: ~/.local/bin/intellij-idea
```


---

## 21. Upgrade flow

Command:

```bash
pako upgrade intellij-idea
```

### 21.1. Resolve

1. read and validate the current receipt;
2. refresh trusted repository metadata;
3. resolve the newest allowed release for the selected channel and target;
4. if the trusted OCI manifest digest equals the installed digest, report that no upgrade is available;
5. fetch and verify the new OCI manifest, package manifest, and pack index;
6. validate compatibility with the current client and target.

### 21.2. Local reuse

For every required chunk:

1. check the verified raw chunk cache;
2. if absent, check the active version's manifest-backed source map;
3. optionally check retained rollback versions;
4. do not mark the chunk usable until SHA-256 of the actual bytes has been verified;
5. discard or quarantine a corrupt cache object and continue planning from another source.

### 21.3. Download

Download only complete packs that contain at least one missing required chunk and are not already present in the verified pack cache.

Example:

```text
total stored data for target release: 2.79 GiB
raw content reusable locally:          97.8%
packs to download:                     61 MiB
```

The user-facing plan must not claim that 97.8% reuse automatically means only 2.2% network transfer, because whole-pack overfetch may make the download larger. Both values should be shown separately.

### 21.4. Materialization and activation

The new release is always created in a new staging directory. The old version directory must never be modified.

After complete verification:

```text
current: 2026.1.0-1 -> 2026.1.1-1
```

The old version remains in `cellar` as a rollback target.

### 21.5. Dirty installation

When the application changed managed files:

- Pako does not automatically discard every local byte;
- every locally sourced chunk is verified independently;
- valid chunks are reused;
- invalid or unavailable chunks are treated as missing;
- the new release is still materialized as a clean tree;
- no modified file is copied wholesale merely because its path and size match.

There is no fallback called "download the full archive" because a full archive is not a separate format. The fallback is to download every pack required by the target release.

### 21.6. Upgrade all packages

```bash
pako upgrade --all
```

The command should:

1. refresh repository metadata once;
2. resolve all installed packages against one trusted snapshot;
3. produce a complete plan before mutating anything;
4. install packages independently, each with its own transaction;
5. continue or stop according to a documented policy when one package fails;
6. return a non-zero exit code if any requested package failed.

A cross-package all-or-nothing transaction is not required for `0.1.0`.

---

## 22. Receipt format: `pako.receipt.v1`

Example:

```json
{
  "schema": "pako.receipt.v1",
  "name": "intellij-idea",
  "upstreamVersion": "2026.1.1",
  "release": 1,
  "target": "linux/x86_64",
  "repository": "core",
  "channel": "stable",
  "ociRepository": "ghcr.io/getpako/core/intellij-idea",
  "ociManifestDigest": "sha256:...",
  "packageManifestDigest": "sha256:...",
  "packIndexDigest": "sha256:...",
  "treeDigest": "sha256:...",
  "versionDirectory": "2026.1.1-1",
  "installedAt": "2026-07-11T12:00:00Z",
  "lastVerifiedAt": null,
  "integrations": {
    "launchers": ["intellij-idea"],
    "desktopEntries": ["pako-intellij-idea-intellij-idea.desktop"],
    "icons": ["pako-intellij-idea"]
  }
}
```

A receipt must not store arbitrary absolute paths that are later trusted without validation. `versionDirectory` is a validated safe component. Full paths are always derived from the current XDG layout and package name.

Atomic receipt write:

```text
write temporary file
    -> flush and fsync file
    -> rename over destination
    -> fsync parent directory
```

Required validation includes:

- exact supported schema;
- valid package name, target, version, release, and digests;
- `versionDirectory` equal to the canonical directory derived from version and release;
- repository name still present or explicitly marked as removed;
- all integration names conform to their restricted formats;
- no duplicate integration ownership entries.

Receipts are local state, not a source of package authenticity. Remote manifests must still be resolved through trusted metadata or fetched by exact digest stored in the receipt.

---

## 23. Transactions and recovery

### 23.1. Transaction journal

Before the first mutation of active state, create:

```text
$XDG_STATE_HOME/pako/transactions/<uuid>.json
```

The journal contains at least:

- transaction schema and UUID;
- operation type;
- package name;
- previous receipt and active version;
- new receipt and version;
- staging and final version directory names;
- exposures to create, replace, or remove;
- backups of copied exposure files where needed;
- current phase;
- creation and update timestamps;
- enough information to roll forward or roll back without network access when possible.

Suggested phases:

```text
prepared
version_committed
exposures_committed
active_link_committed
receipt_committed
complete
```

Every phase transition is written atomically and durably before the next irreversible action.

### 23.2. Startup recovery

Before any mutating command performs its requested operation, it must:

1. scan incomplete transaction journals;
2. acquire the locks required by each transaction;
3. validate all referenced paths against the current XDG layout;
4. either restore the previous consistent state or finish the new commit if every required new object is complete and verified;
5. retain evidence required for recovery until completion is durable;
6. make recovery idempotent so repeating it produces the same final state.

The recovery policy for each phase must be documented and unit-tested.

### 23.3. Locks

Required lock classes:

- per-package exclusive lock for install, upgrade, remove, rollback, and repair;
- repository metadata lock for refresh and configuration changes;
- global exposure lock while checking or modifying launchers, desktop entries, icons, and ownership state;
- per-digest cache lock while downloading or publishing the same pack or chunk;
- optional global cleanup lock while sweeping shared cache state.

Different packages may download and materialize concurrently. Their exposure commit stages are serialized.

Lock files must include diagnostic information such as process ID, operation, and start time, but stale-lock detection must be based on operating-system locks rather than blindly deleting files by age.

### 23.4. Filesystem durability

The implementation must document where `fsync` is required. At minimum, durable transaction semantics require appropriate syncing of:

- completed downloaded objects before final rename;
- materialized version contents before activation;
- transaction journals after phase changes;
- receipt files;
- symlink parent directories after atomic replacement;
- exposure files and parent directories.

Tests may use a configurable durability layer so fault injection can simulate failures between operations.

---

## 24. Protection against application self-updaters

### 24.1. Immutable payload

After materialization, use normalized permissions such as:

```text
regular non-executable file: 0444
regular executable file:     0555
directory:                   0555
```

This reduces accidental mutation but is not a security boundary against a process running as the same user, because that process may restore write permissions.

### 24.2. User state separation

Configuration, plugins, caches, logs, projects, and other mutable user state must live outside the version directory according to the application's supported mechanisms and XDG conventions.

A recipe must not place expected mutable state inside the managed payload. Where an application requires a writable directory under its installation root, the package must either:

- redirect it through documented vendor configuration;
- declare a narrowly scoped managed mutable path model introduced by a future format;
- be rejected for `0.1.0` if it cannot operate safely under the immutable model.

### 24.3. Vendor-specific updater policy

A recipe may include declarative configuration that disables the vendor updater when an official supported mechanism exists. Examples include startup properties, environment variables, or configuration files documented by the vendor.

The client must not patch application binaries or use fragile undocumented modifications merely to suppress updates.

### 24.4. Verification model

- `pako verify` detects missing, changed, and unexpected entries;
- an upgrade verifies each local chunk when it is read;
- modified data cannot contaminate the new installation;
- `pako repair` reconstructs a clean copy from verified local and remote chunks;
- launchers may display a warning when a previously verified installation is known to be dirty, but verification status must not be guessed from mtime alone.

### 24.5. Expected behavior for JetBrains IDEs

For IntelliJ IDEA and Android Studio, the package should preserve vendor-managed user directories outside `cellar`. Pako owns only the application installation tree. Built-in application updates must be disabled through supported settings where practical. Plugins remain user state and are not included in package tree verification unless a future explicit plugin-management feature is added.

---

## 25. Command-line interface

Target command set for `0.1.0`:

```text
pako repo add <name> <oci-url> --root <file> [--yes]
pako repo remove <name> [--yes]
pako repo list
pako repo update [<name>]

pako search <query>
pako info <package>
pako install <package>[@<version>] [--channel <channel>] [--yes]
pako upgrade [<package>] [--all] [--dry-run] [--yes]
pako list
pako status [<package>]
pako verify <package>
pako repair <package> [--yes]
pako rollback <package> [--to <version-release>] [--yes]
pako remove <package> [--yes]
pako cleanup [<package>] [--keep <count>] [--cache] [--dry-run] [--yes]
pako doctor
pako run <package> [--launcher <name>] -- [arguments...]
```

### 25.1. `status`

`status` is a fast receipt-based operation. It must not hash a multi-gigabyte installation automatically.

It displays:

- active release;
- repository and channel;
- exact OCI and package manifest digests;
- last full verification result and timestamp;
- retained rollback candidates;
- known incomplete transaction state;
- whether an upgrade is known from already cached metadata.

### 25.2. `verify`

Performs a full scan of the package tree and exposures. Any difference returns a non-zero exit status.

### 25.3. `repair`

Reconstructs exactly the release recorded in the receipt unless the user explicitly requests an upgrade instead.

### 25.4. `rollback`

Fully verifies the target retained version before activation. No network is required when the retained version and its local manifest are complete.

### 25.5. `cleanup`

By default retains one inactive version. Shared cache cleanup is separate or explicitly enabled with `--cache`.

### 25.6. `doctor`

Checks:

- XDG layout and permissions;
- incomplete transactions;
- invalid receipts;
- broken `current` symlinks;
- missing or conflicting exposures;
- unsupported filesystem behavior;
- free disk space;
- system clock plausibility for TUF expiration;
- repository reachability;
- ability to perform atomic rename in required directories.

### 25.7. `--json`

All read-only and planning commands should support stable, versioned JSON output. Machine-readable data goes to stdout; progress and diagnostics go to stderr.

Example top-level shape:

```json
{
  "schema": "pako.cli.v1",
  "command": "upgrade-plan",
  "result": {}
}
```

Human-readable output must not be parsed internally by other Pako components.

---

## 26. Exposure installation and ownership

### 26.1. Ownership

Every exposure created by Pako must have one logical owner recorded in durable state.

Before replacing a destination under the global exposure lock:

1. inspect active receipts and the ownership index;
2. if the destination does not exist, creation is allowed;
3. if it belongs to the package being updated, replacement is allowed;
4. if it belongs to another package, report a conflict;
5. if it is unmanaged, do not overwrite it without explicit user action;
6. verify that the actual destination path has not become an unsafe symlink.

Ownership state must be reconstructible from receipts. A separate `ownership.json` is an index and may not be the only source of truth.

### 26.2. Launchers

Preferred model:

```text
~/.local/bin/intellij-idea
    -> small Pako-controlled wrapper
    -> resolves apps/<package>/current/bin/idea
```

The wrapper must:

- use safe shell quoting or be a compiled/symlink-based launcher;
- not embed the current version directory;
- propagate arguments and exit status;
- avoid interpreting user-supplied arguments;
- work when `$HOME` contains spaces or non-ASCII characters.

An alternative is a stable launcher invoking:

```text
pako run <package> --launcher <id> -- "$@"
```

The implementation should choose one primary model and test it thoroughly.

### 26.3. Desktop entries and icons

Desktop entries are rendered from structured manifest data and written atomically. The client must not install arbitrary unvalidated `.desktop` files from package content.

Icon destinations must use package-prefixed names to avoid collision. Supported image formats and size labels must be explicitly validated.

### 26.4. Desktop database refresh

Running `update-desktop-database` or icon-cache tools is best effort. Failure of an optional helper does not invalidate an otherwise correct installation, but it must be reported as a warning and surfaced by `doctor` when relevant.

---

## 27. Verification and repair

### 27.1. `verify`

Verification covers:

- receipt schema and cross-field consistency;
- existence and type of the version directory;
- `current` symlink destination;
- all declared directories, files, and symlinks;
- absence of undeclared entries;
- exact regular-file sizes and SHA-256 digests;
- tree digest;
- normalized executable modes;
- launchers;
- desktop entries;
- icons;
- exposure ownership.

Problems are classified using stable machine-readable codes:

```text
missing
modified
unexpected
mode_changed
symlink_changed
exposure_missing
exposure_modified
exposure_conflict
receipt_invalid
active_link_invalid
manifest_unavailable
```

Verification must never repair state implicitly.

### 27.2. `repair`

Repair procedure:

1. acquire the package lock and recover previous transactions;
2. fetch package manifest and pack index by exact digests stored in the receipt;
3. verify the manifests against trusted or previously trusted exact metadata;
4. identify locally valid chunks;
5. download missing packs;
6. materialize a completely new staging tree;
7. verify all files and the tree digest;
8. transactionally activate it as the same package release;
9. restore exposures;
10. move the damaged directory to quarantine or remove it only after successful activation.

Repair must not patch files in place.

### 27.3. Offline repair

When every required chunk is available from retained versions or caches and the exact manifests are stored locally, repair should work offline. If content is missing, the command reports the precise required network data.

---

## 28. Rollback and version retention

After an upgrade, the default retained set is:

```text
current version
one previous version
```

The package manifests and receipts needed to verify both versions must also remain available locally.

Rollback procedure:

1. select an explicitly requested or most recent retained version;
2. load its exact local manifest;
3. run full verification;
4. prepare matching exposures;
5. transactionally switch `current`, receipt, and integrations;
6. keep the version that was active before rollback as another candidate until cleanup removes it.

Never automatically delete the old directory immediately after an upgrade. A running application may lazily load files from its old installation directory.

A rollback target that fails verification must not be activated automatically. The user may run `repair` for that exact release if manifests and content remain available.

---

## 29. Cleanup and garbage collection

### 29.1. Installed versions

```bash
pako cleanup intellij-idea --keep 1
```

removes inactive versions beyond the retention count.

Before deletion:

- the version must not be active;
- no open transaction may reference it;
- no receipt may require it;
- it must not be selected as a protected rollback target;
- the user must receive a dry-run plan with reclaimable bytes;
- deletion must not follow symlinks outside the version root.

### 29.2. Shared cache

Cache cleanup uses mark-and-sweep rather than fragile reference counters.

Mark phase:

- read active and retained receipts;
- read locally retained package manifests and pack indexes;
- mark chunks and packs required by protected installations;
- mark objects currently used by open transactions or downloads;
- optionally mark recently used cache objects according to retention policy.

Sweep phase:

- remove only unmarked objects older than a safety grace period;
- hold the appropriate cache lock;
- tolerate objects disappearing concurrently after rechecking locks;
- report reclaimed bytes and object counts.

Example:

```text
Referenced raw chunks:    4.02 GiB
Referenced packs:          612 MiB
Unreferenced cache:         94 MiB
Reclaimable:                94 MiB
```

Deleting all cache must not affect the ability to run an installed application or roll back to a retained complete version.

### 29.3. Quarantine

Corrupt cache objects and damaged version directories may be moved into a bounded quarantine area for diagnostics. Quarantine is not indefinite retention; cleanup removes it after a documented grace period.


---

## 30. Recipes and publishing pipeline

The normative recipe specification is maintained in
[`docs/RECIPE_FORMAT.md`](docs/RECIPE_FORMAT.md). This section defines the
architectural requirements that the implementation must preserve.

Reference recipes:

- [`examples/intellij-idea/recipe.toml`](examples/intellij-idea/recipe.toml) — prebuilt vendor archives with different sources per architecture;
- [`examples/source-build/recipe.toml`](examples/source-build/recipe.toml) — sandboxed source compilation workflow.

### 30.1. Trust boundary

A recipe is trusted build-side code and data. It is used only by `pako-build` in
the publishing pipeline. The normal Pako client consumes only signed metadata,
declarative package manifests, pack indexes, and verified chunk data.

The client must never execute recipe scripts during install, upgrade, repair,
rollback, cleanup, or remove.

### 30.2. Recipe version

Every recipe starts with a numeric schema version:

```toml
schema = 1
```

The format must not use `pako.recipe.v1` inside the TOML document. Document type
is already known from the filename and parser. Namespace-qualified media types
remain appropriate for generated OCI artifacts.

### 30.3. No update-discovery format

Pako does not define release scrapers or provider-specific update logic. The `getpako/packages` repository may contain Python scripts that discover
new upstream versions, update exact URLs, sizes, and checksums, and open pull
requests. This automation is repository infrastructure, not a Pako client or
recipe feature.

`pako-build` always receives an exact recipe. It must not interpret `latest`,
`current`, a floating branch, or an unpinned download as a release identity.

### 30.4. Target-specific sources

Each architecture may define different sources:

```toml
[[targets]]
target = "linux/x86_64"

[targets.build]
kind = "prebuilt"
payload_root = "payload"

[[targets.sources]]
id = "upstream"
kind = "archive"
urls = ["https://example.org/app-linux-x86_64.tar.gz"]
sha256 = "..."
size = 123456789
format = "tar.gz"
destination = "payload"
strip_components = 1

[[targets]]
target = "linux/aarch64"

[targets.build]
kind = "prebuilt"
payload_root = "payload"

[[targets.sources]]
id = "upstream"
kind = "archive"
urls = ["https://example.org/app-linux-aarch64.tar.gz"]
sha256 = "..."
size = 120000000
format = "tar.gz"
destination = "payload"
strip_components = 1
```

This is the normal design for vendor applications such as IntelliJ IDEA and
Android Studio. Only sources for the requested target are downloaded.

### 30.5. Prebuilt packages

A prebuilt recipe downloads already compiled upstream binaries. The builder:

1. validates the recipe and target;
2. downloads exact sources;
3. verifies declared sizes and SHA-256 digests;
4. safely extracts the archive;
5. applies transformations;
6. validates assertions and tests;
7. audits the final payload;
8. performs FastCDC chunking and pack creation.

It does not run an upstream installer merely because the archive contains one.

### 30.6. Source packages

Source packages may execute shell scripts during the build. This is required for
applications such as VSCodium and must be supported by schema `1`.

Supported build phases:

```text
prepare -> configure -> build -> check -> install
```

Conceptual configuration:

```toml
[targets.build]
kind = "source"
environment = "ghcr.io/getpako/build-images/linux-x86_64-v1@sha256:..."
shell = "bash"
network = false
timeout_seconds = 7200
payload_root = "dest"

[targets.build.scripts]
configure = """
cmake -S "$PAKO_SOURCE_DIR" -B "$PAKO_BUILD_DIR" -G Ninja \
  -DCMAKE_INSTALL_PREFIX=/
"""

build = """
cmake --build "$PAKO_BUILD_DIR" --parallel "$PAKO_JOBS"
"""

check = """
ctest --test-dir "$PAKO_BUILD_DIR" --output-on-failure
"""

install = """
DESTDIR="$PAKO_DESTDIR" cmake --install "$PAKO_BUILD_DIR"
"""
```

The `install` phase must place the final package tree only under
`$PAKO_DESTDIR`.

### 30.7. Build sandbox

Recipe scripts are executable code and must never run directly on the publishing
host. The sandbox must provide:

- no root privileges;
- no publishing or registry credentials;
- no container runtime socket;
- no host home directory;
- only declared read-only inputs;
- isolated writable source, build, destination, home, and temporary directories;
- network disabled by default;
- CPU, memory, process, disk, and time limits;
- a build environment identified by immutable OCI digest.

The publishing coordinator receives only the audited output tree and build
report from the sandbox. Credentials are added only after untrusted build code
has finished.

### 30.8. Build-script environment

The stable environment contract includes:

```text
PAKO_RECIPE_DIR
PAKO_SOURCE_DIR
PAKO_BUILD_DIR
PAKO_DESTDIR
PAKO_TARGET
PAKO_OS
PAKO_ARCH
PAKO_JOBS
PAKO_PACKAGE_NAME
PAKO_PACKAGE_VERSION
PAKO_PACKAGE_RELEASE
SOURCE_DATE_EPOCH
HOME
TMPDIR
```

`HOME` and `TMPDIR` always point inside the sandbox.

### 30.9. Transformations and integrations

Recipes may use structured transformations such as remove, move, copy, chmod,
write, symlink, and patch. Global transformations apply to every target;
target-specific transformations run afterwards.

Launchers, desktop entries, and icons remain structured data. Arbitrary client
scriptlets for integration are forbidden.

### 30.10. Publishing pipeline

1. read and validate `recipe.toml`;
2. select exactly one requested target;
3. fetch and verify all declared inputs;
4. prepare sources in an isolated workspace;
5. run prebuilt extraction or source build phases;
6. apply transformations;
7. run assertions and build-side tests;
8. audit the final payload and reject unsafe entries;
9. calculate file digests and tree digest;
10. perform FastCDC chunking;
11. reuse already published chunks where available;
12. create immutable packs containing only new chunks;
13. generate package manifest, pack index, and build report;
14. reconstruct the package locally from generated artifacts and verify it;
15. publish OCI blobs and manifests;
16. publish signed repository metadata only after every artifact is available.

A mandatory double-build reproducibility check is intentionally not part of
`0.1.0`. Packaging from a given final payload tree must still be deterministic.

### 30.11. Redistribution rights

The recipe records license and redistribution status. The official publisher
must not upload vendor binaries when redistribution is not confirmed. Build and
lint may still run locally to validate the recipe.

## 31. Input limits

All repository and package data remains untrusted until fully verified.

Minimum `v1` limits:

| Item | Limit |
| --- | ---: |
| package manifest | 16 MiB |
| pack index | 32 MiB |
| OCI manifest or index | 8 MiB |
| pack hard limit | 32 MiB |
| tree entries | 500,000 |
| unique chunks | 2,000,000 |
| path length | 4,096 bytes |
| path component length | 255 bytes |
| single chunk | profile limit, maximum 4 MiB |
| default maximum installed size | 100 GiB |
| desktop entries per package | 128 |
| launchers per package | 128 |
| icons per package | 512 |
| symlink target length | 4,096 bytes |

Exceeding a limit aborts the operation before uncontrolled allocation, decompression, or disk writes.

Parsers must use checked arithmetic for:

- offsets and lengths;
- count multiplied by entry size;
- accumulated installed size;
- accumulated chunk sizes;
- decompression output sizes;
- filesystem-space estimates.

Limits should be configurable only through explicitly documented development or administrator settings. A package must not be able to raise its own limits.

---

## 32. Networking and downloads

The client should use a native Rust HTTP implementation rather than invoking external `curl`.

Requirements:

- HTTPS by default;
- bounded redirect count;
- connection, request, and idle timeouts;
- retries only for operations safe to repeat;
- exponential backoff with jitter;
- complete-blob resume for pack downloads;
- no trust in resumed metadata without full final verification;
- size limits before and during transfer;
- digest verification after completion;
- no authentication tokens in logs, errors, process arguments, or URLs;
- anonymous access for public GHCR packages;
- private-registry support as an extension rather than a release blocker;
- proxy behavior delegated to a documented HTTP-client configuration;
- TLS certificate validation enabled and not bypassed by package metadata.

### 32.1. Resumable downloads

A partial download record must bind:

```text
registry
repository
blob digest
expected size
current size
validator information where available
```

Before resuming, validate that the partial file size is plausible. After completion, calculate SHA-256 over the entire blob regardless of transport validators.

### 32.2. HTTP Range

Downloading individual chunk ranges is intentionally deferred. The pack index already includes offsets so a future client can request ranges without changing package manifests.

When Range support is added, it must gracefully fall back to full-pack downloading when:

- the registry does not support Range;
- authentication or CDN behavior prevents reliable ranges;
- the requested range response is invalid;
- the economic planner determines a full pack is cheaper.

---

## 33. Error model and exit codes

Use typed domain errors, for example with `thiserror`, while preserving causal chains and adding operation context.

Top-level categories:

```text
Usage
NotFound
Trust
Network
Integrity
Conflict
Transaction
Unsupported
Internal
```

Recommended exit codes:

| Code | Meaning |
| ---: | --- |
| 0 | success |
| 2 | invalid CLI usage |
| 3 | package or release not found |
| 4 | trust, signature, rollback, or expiration failure |
| 5 | network or registry failure |
| 6 | integrity or format verification failure |
| 7 | exposure, ownership, or state conflict |
| 8 | transaction or recovery failure |
| 9 | unsupported platform, format, or feature |
| 1 | other failure |

A user-facing error should state:

- what failed;
- which package and operation phase were affected;
- whether the previous version remains active;
- whether any transaction will be recovered automatically;
- one safe next command where appropriate.

Do not print a full backtrace outside explicit debug mode.

Machine-readable JSON errors must include stable codes, not only free-form text.

---

## 34. Logging and user experience

Default output should be concise and informative:

```text
Resolving intellij-idea 2026.1.1-1...
Download: 61 MiB in 4 packs
Reuse: 97.8% of required raw chunk bytes
Installed size: 3.96 GiB

✓ Verified repository metadata
✓ Downloaded and verified 4 packs
✓ Materialized 12,491 files
✓ Verified package tree
✓ Activated 2026.1.1-1
✓ Preserved 2026.1.0-1 for rollback
```

Rules:

- progress goes to stderr;
- `--json` data goes to stdout;
- `--quiet` suppresses non-essential output;
- `--verbose` shows digests, selected sources, and transaction phases;
- debug logging is enabled through a documented option or `RUST_LOG`;
- ANSI is disabled when the stream is not a terminal or `NO_COLOR` is set;
- byte counts use binary units consistently;
- interrupted operations explain whether they are safe to retry;
- secrets and authorization headers are always redacted.

Progress bars must not be part of the domain layer and must remain usable in non-interactive environments.

---

## 35. Complete installation and upgrade example

### 35.1. First installation

```bash
pako install intellij-idea
```

Plan:

```text
Repository: core
Version: 2026.1.0-1
Target: linux/x86_64
Required chunks: 3,842
Required packs: 176
Download: 2.74 GiB
Installed size: 3.96 GiB

Proceed? [Y/n]
```

No installed version exists, so Pako downloads all packs not already present in cache.

It then:

1. verifies signed repository metadata;
2. resolves the exact OCI digest;
3. verifies package manifest and pack index;
4. downloads and verifies all required packs;
5. extracts and verifies chunks;
6. materializes the package into staging;
7. verifies every file and the tree digest;
8. atomically commits the version;
9. installs launchers and desktop integration;
10. activates the version through `current`.

Result:

```text
$XDG_DATA_HOME/pako/cellar/intellij-idea/2026.1.0-1/
$XDG_DATA_HOME/pako/apps/intellij-idea/current
    -> ../../cellar/intellij-idea/2026.1.0-1
$HOME/.local/bin/intellij-idea
```

### 35.2. Publishing the next release

The old release logically uses:

```text
A B C D E F
```

The new release uses:

```text
A B X D E Y
```

The publisher discovers that `A`, `B`, `D`, and `E` already exist in old immutable packs. It publishes only chunks `X` and `Y` in one or more new, possibly small packs.

The new release manifest references both old and new packs.

### 35.3. Upgrade

```bash
pako upgrade intellij-idea
```

Planner result:

```text
Current version: 2026.1.0-1
Target version:  2026.1.1-1
Required chunks: 3,901
Available from cache or installed versions: 3,824
Missing chunks: 77
New packs to download: 4
Network download: 61 MiB
Full stored release data: 2.79 GiB
Raw content reuse: 97.8%
```

Pako:

1. downloads four packs;
2. verifies complete pack digests and structures;
3. verifies every new chunk;
4. reads and verifies reusable chunks from the old installation or cache;
5. builds `2026.1.1-1` in a new staging directory;
6. verifies file digests and the tree digest;
7. atomically switches `current`;
8. records the new receipt;
9. retains `2026.1.0-1` for rollback.

### 35.4. Dirty old installation

Suppose the application's own updater changed `lib/platform.jar`.

Pako never copies that file wholesale. It verifies each expected old chunk range. Matching chunks are reused; mismatching chunks are treated as missing and obtained from packs. The resulting `2026.1.1-1` tree is still identical to the signed target manifest.

### 35.5. Rollback

```bash
pako rollback intellij-idea
```

Pako fully verifies `2026.1.0-1`, restores matching exposures, and atomically switches the active symlink and receipt. No package download is required.

### 35.6. Repair

```bash
pako repair intellij-idea
```

If 12 chunks are damaged and all other content remains reusable:

```text
Valid local chunks: 3,889
Missing or damaged chunks: 12
Download: 9.4 MiB
```

Pako reconstructs the same release in a new directory and transactionally activates the clean copy.

---

## 36. Tests required before `0.1.0`

### 36.1. Unit tests

- package name, version, target, and path validation;
- architecture normalization;
- canonical serialization;
- tree digest vectors;
- FastCDC deterministic vectors;
- small-file threshold boundaries;
- pack encoding and decoding;
- pack overflow and malformed indexes;
- package-manifest and pack-index cross-validation;
- receipt validation;
- download planning;
- ownership conflicts;
- transaction-journal state machine;
- error-to-exit-code mapping.

### 36.2. Property tests

- arbitrary chunk sequences survive pack encode/decode byte-for-byte;
- manifest serialize/deserialize preserves canonical meaning;
- generated validated paths never escape the root;
- checked arithmetic never permits out-of-file ranges;
- random transaction interruption never leaves two authoritative active receipts;
- reconstruction from a valid manifest produces exactly the expected digest;
- arbitrary duplicate or reordered entries are rejected.

### 36.3. Fuzzing

Fuzz targets:

- package manifest parser;
- pack index parser;
- `PAKPACK1` parser and decompressor boundary checks;
- OCI manifest and platform selection;
- path normalization;
- desktop entry rendering;
- receipt parser;
- journal recovery.

### 36.4. Integration tests

- installation from a local OCI registry;
- upgrade with chunk reuse;
- upgrade across multiple skipped releases;
- modified file in the active installation;
- missing raw chunk cache;
- corrupt raw chunk cache;
- corrupt pack cache;
- interrupted download and resume;
- rollback without network access;
- repair of the same release;
- launcher conflict between two packages;
- recovery after failure in every commit phase;
- concurrent installation of different packages;
- concurrent operations on the same package;
- XDG paths containing spaces and Unicode;
- repository metadata expiration and refresh;
- architecture selection from a multi-platform OCI index.

### 36.5. Security tests

- `..` traversal and absolute paths;
- symlink-ancestor attack;
- symlink target escape;
- duplicate and aliased entries;
- decompression bomb;
- false raw size;
- chunk, file, pack, and tree digest mismatch;
- modified TUF timestamp;
- expired metadata;
- metadata rollback;
- mix-and-match snapshot attack;
- OCI tag moved to another digest;
- unmanaged destination exposure;
- malicious receipt path fields;
- authentication-token redaction.

### 36.6. Fault injection

Force failure after every relevant step, including:

```text
after journal creation
after a pack is downloaded but before rename
after staging verification
after staging -> cellar rename
after an exposure is created
after current is switched
after receipt write
before journal deletion
```

After restart, recovery must converge to one consistent state.

### 36.7. Benchmarks

Using at least two consecutive releases of multiple large applications, measure:

- chunking throughput;
- materialization throughput;
- peak memory;
- chunk count;
- package manifest size;
- pack index size;
- pack count and size distribution;
- raw reuse ratio;
- actual downloaded bytes;
- pack overfetch when only one chunk is required;
- time spent verifying installed sources;
- cold-cache and warm-cache behavior.

Before freezing `pako-fastcdc-v1`, compare average chunk sizes of 512 KiB, 1 MiB, and 2 MiB. The current default remains 1 MiB unless benchmarks demonstrate a clearly better tradeoff.

---

## 37. CI and code quality

Required CI commands:

```text
cargo fmt --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo audit
cargo deny check
```

Additionally:

- test the declared minimum supported Rust version when one is established;
- run native tests on Linux x86_64;
- compile and preferably test Linux aarch64;
- run an integration test against a local OCI registry;
- run fuzz targets periodically and before format releases;
- prohibit `unsafe` unless separately justified, documented, and tested;
- use `#![forbid(unsafe_code)]` in crates that do not need it;
- document public types and non-obvious invariants with rustdoc;
- fail CI when generated schemas, examples, or compatibility vectors differ from committed files;
- use dependency review and license policy;
- pin critical format-affecting dependency versions.

The repository should include a test matrix that distinguishes fast pull-request checks from slower nightly security and benchmark jobs.

---

## 38. Implementation map and removal of legacy experiments

The repository implements the chunked architecture directly. Historical full-archive and pairwise version-to-version delta experiments are intentionally absent and must not be reintroduced as a parallel public system.

### 38.1. Remove

- the experimental full-archive package format;
- the experimental pairwise delta format;
- `build-delta` command;
- `PARCDLT1` patch bytecode;
- Dijkstra or other graph search over delta edges;
- repository index fields dedicated to pairwise deltas;
- full archive as a separate fallback artifact type;
- compatibility with legacy receipts and package archives;
- code paths that mutate an installation tree in place.

### 38.2. Preserve conceptually

- XDG layout;
- versioned installation directories;
- staging;
- read-only payload;
- atomic activation through a symlink;
- rollback;
- exposure ownership protection;
- path validation;
- atomic state writes;
- status, verify, repair, and cleanup;
- locks and recovery, after redesign around the new transaction model.

### 38.3. Replace modules

| Current area | Target area |
| --- | --- |
| `delta.rs` | `chunking`, `pack`, `pack_index`, `download_planner` |
| `artifact.rs` | `package_manifest`, `tree_digest`, `materialize` |
| `repo.rs` | trusted catalog plus OCI registry client |
| `resolver.rs` | version, channel, repository, and target resolver without a delta graph |
| `pako_manifest.rs` | `pako.package-manifest.v1` |
| `build.rs` | separate `pako-build` pipeline |
| `installer.rs` | transaction coordinator plus chunk sources |
| `integrity.rs` | chunk, file, tree, and exposure verification reports |
| `receipt.rs` | `pako.receipt.v1` without arbitrary trusted paths |

### 38.4. Development-state handling

Because no public version exists, the new client may reject legacy development state with a clear message:

```text
Unsupported pre-0.1.0 Pako state detected.
Remove the development state or run:
  pako doctor --reset-development-state
```

Do not implement a production migration that permanently preserves the old formats.

### 38.5. Refactoring discipline

The agent must not rewrite the entire project in one unreviewable change. Each implementation stage should:

- keep the workspace compiling;
- include tests for new behavior;
- remove obsolete code once no longer referenced;
- update documentation and examples in the same commit;
- avoid temporary compatibility abstractions not required before `0.1.0`.

---

## 39. Required implementation order

The coding agent should work in stages. After every stage, the codebase must compile and tests for that stage must pass.

### Stage 1 — models and validation

Implement:

- workspace crates;
- `Sha256Digest` as a strong type;
- safe package name, version, release, target, and relative path types;
- canonical JSON support;
- package manifest `v1`;
- tree digest `v1`;
- receipt `v1`;
- serialization and validation tests.

**Acceptance criterion:** a static directory tree can be described, serialized, deserialized, and verified without network access.

### Stage 2 — FastCDC

Implement:

- `Chunker` abstraction;
- `pako-fastcdc-v1` profile;
- streaming chunking;
- independent file and chunk SHA-256;
- deterministic compatibility vectors;
- synthetic benchmarks.

**Acceptance criterion:** an input tree produces a deterministic manifest containing valid ordered chunk lists.

### Stage 3 — packfile

Implement:

- `PAKPACK1` encoder and decoder;
- independent Zstandard frames;
- deterministic pack builder with 16 MiB soft target;
- pack index;
- full cross-validation;
- property and fuzz tests.

**Acceptance criterion:** a complete tree can be reconstructed using only package manifest, pack index, and packs.

### Stage 4 — materialization and local sources

Implement:

- raw chunk cache;
- complete-pack cache;
- installed-version chunk source;
- download planner;
- secure staging materializer;
- file and tree verification.

**Acceptance criterion:** upgrading a synthetic release reads or downloads only missing chunks and produces the exact target tree.

### Stage 5 — transactional installer

Implement:

- XDG layout;
- package locks;
- exposure lock and ownership;
- transaction journal;
- atomic `current` switch;
- receipt commit;
- recovery and fault injection;
- rollback and installed-version cleanup.

**Acceptance criterion:** termination at any commit point never leaves an unrecoverable or ambiguous active installation.

### Stage 6 — OCI

Implement:

- native OCI registry client;
- manifest and index resolution;
- blob download with resume;
- digest and size verification;
- local-registry integration tests;
- publisher upload.

**Acceptance criterion:** `pako-build` publishes a package and a clean client installs it from an OCI registry without external container tools.

### Stage 7 — trusted metadata

Implement:

- root bootstrap;
- timestamp, snapshot, and targets refresh;
- signature, expiration, and rollback checks;
- signed package catalog;
- repository commands.

**Acceptance criterion:** moving an OCI tag or replacing unsigned catalog data cannot redirect an installation.

### Stage 8 — complete CLI and user operations

Implement:

- install, upgrade, and dry-run;
- status, verify, and repair;
- rollback, remove, and cleanup;
- doctor;
- stable JSON output;
- progress, prompts, and exit codes.

### Stage 9 — real recipes and profile freeze

Implement and test:

- one small redistributable test application;
- IntelliJ IDEA where redistribution terms permit;
- Android Studio where redistribution terms and stable upstream sources permit;
- multi-release benchmarks;
- final chunk-profile decision and committed compatibility vectors.

**Acceptance criterion:** at least one real large application completes install, upgrade, verify, repair, rollback, and cleanup through the full trusted OCI path.

---

## 40. Definition of Done for `0.1.0`

The release may be tagged `0.1.0` only when all items below are complete:

- [ ] installation requires no administrator privileges;
- [ ] the client executes no code from a package during installation;
- [ ] a package resolves through signed metadata to an exact OCI digest;
- [ ] the format supports `linux/x86_64` and `linux/aarch64`;
- [ ] FastCDC and packfile `v1` profiles are frozen and documented;
- [ ] first installation works with an empty cache;
- [ ] upgrade reuses verified chunks from the previous installation;
- [ ] a small upstream change does not require downloading the complete application;
- [ ] a corrupt local chunk is detected and reacquired;
- [ ] a corrupt pack is rejected;
- [ ] every file digest and tree digest are checked before activation;
- [ ] activation is transactional and has crash recovery;
- [ ] rollback works without network access for a retained valid version;
- [ ] repair does not modify the active tree in place;
- [ ] a self-modified installation cannot contaminate an upgrade;
- [ ] exposure conflicts are detected before overwrite;
- [ ] cleanup never removes an active or transaction-required version;
- [ ] manifest and pack parsers enforce limits and have fuzz coverage;
- [ ] CI passes with Clippy warnings denied;
- [ ] format documentation matches the implementation and generated schemas;
- [ ] at least one real large application passes install, upgrade, verify, repair, rollback, and cleanup;
- [ ] actual update download size and pack overfetch have been benchmarked;
- [ ] README and examples describe only the chunked Pako distribution model;
- [ ] repository metadata expiration, rollback, and tag-move tests pass;
- [ ] a clean machine can install using only Pako and network access to the configured repository.

---

## 41. Features intentionally deferred until after `0.1.0`

The following features must not block the first release:

- downloading one chunk through HTTP Range;
- mandatory double-build, bit-for-bit source reproducibility verification;
- multiple alternative remote locations for one chunk;
- mirror selection;
- peer-to-peer transfer;
- remote deduplication across unrelated packages;
- complete dependency solving and system dependency management;
- application sandboxing through Landlock or Bubblewrap;
- a system daemon;
- automatic background updates;
- staged rollouts and cohorts;
- reflink-based whole-file materialization;
- advanced private-registry credential helpers;
- GUI;
- native management of plugins inside third-party IDEs;
- system-wide installation mode;
- macOS or Windows support.

Format `v1` should still leave room for HTTP Range, mirrors, additional chunking profiles, and future storage planners without changing the basic package-tree manifest.

---

## 42. Summary of expected behavior

Pako `0.1.0` does not treat a "full package" and a "delta file" as two different distribution formats.

It always performs:

```text
signed repository metadata
    -> exact OCI manifest digest
    -> package manifest + pack index
    -> required chunks
    -> verified local chunk sources
    -> missing immutable packs
    -> new immutable version directory
    -> file digest + tree digest verification
    -> transactional activation
```

For a first installation, nearly all chunks are missing.

For an upgrade, most chunks are available from the previous installation or cache, so only small packs containing new content are downloaded.

For repair, valid local chunks are retained and only missing or damaged content is reacquired.

For rollback, a retained complete version is verified and reactivated without rebuilding or downloading it.

This is the only target distribution and update model for Pako before its first public release.
