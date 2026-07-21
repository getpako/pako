# Pako

Pako is a user-space package manager for large, self-contained Linux applications.

It installs applications without `sudo` and atomically switches the active
version only after complete integrity verification.

> Pako is under active development. The target for the first public release is `0.1.0`.

## Architecture

A package release is represented by a canonical file-tree manifest and one
SHA-256-addressed `payload.tar.zst` archive. The manifest and payload are
distributed as OCI blobs, and signed TUF metadata resolves package releases to
immutable OCI manifests. Pako safely extracts the payload into a staging
directory, verifies every declared entry and the final tree digest, then
atomically activates the completed version.

## Workspace

- `pako-core` — manifests, payload extraction, verification, transactions, integrations and receipts.
- `pako-cli` — the end-user `pako` command.
- `pako-build` — recipe parser, package builder and publisher.
- `pako-oci` — OCI Distribution API client.
- `pako-trust` — TUF-backed release resolution.
- `pako-test-support` — deterministic fixtures and temporary XDG layouts.
- `pako-log` — shared structured logging and coordinated progress rendering.
- `xtask` — development-only helper commands.

## Built-in command documentation

Both command-line programs provide complete help at the root and command level:

```bash
pako --help
pako install --help
pako upgrade --help
pako verify --help
pako rollback --help
pako versions --help
pako prune --help
pako remove --help
pako list --help
pako status --help
pako recover --help

pako-build --help
pako-build lint --help
pako-build build --help
pako-build publish --help
pako-build tuf --help
```

Each command documents its behavior, arguments, safety properties, side effects,
and representative examples.

## Development

```bash
cargo fmt --all --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Build the user CLI and publisher:

```bash
cargo build --release -p pako-cli -p pako-build
```

The resulting binaries are `target/release/pako` and `target/release/pako-build`.

### Offline `pako-build` smoke test

`examples/hello-local` contains a tiny shell script packaged from the repository.
It uses a local `path` source resolved relative to `recipe.toml`, so it neither
downloads sources nor runs a build sandbox:

```bash
cargo run -p pako-build -- build examples/hello-local/recipe.toml \
  --target linux/x86_64 \
  --output /tmp/pako-hello-local
```

The resulting payload contains `bin/hello-pako`. Local source paths must remain
inside the recipe directory. Their SHA-256 digest is calculated automatically while
remote sources must provide an explicit checksum.

## License

Licensed under either Apache License 2.0 or the MIT license, at your option.
