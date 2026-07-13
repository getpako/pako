# Pako

Pako is a user-space package manager for large, self-contained Linux applications.
It installs applications without `sudo`, reuses unchanged content between releases,
and atomically switches the active version only after complete integrity verification.

> Pako is under active development. The target for the first public release is `0.1.0`.

## Architecture

A package release is represented by a canonical file-tree manifest. Regular files are
split into SHA-256-addressed chunks using the frozen `pako-fastcdc-v1` profile. New
chunks are compressed independently and grouped into immutable `PAKPACK1` packfiles,
which are distributed as OCI blobs. Installation and upgrade use the same code path:
Pako downloads only packs containing chunks that are not already available locally.

The complete design and implementation requirements are documented in [Pako.md](Pako.md).

## Workspace

- `pako-core` — manifests, chunking, packfiles, materialization, transactions and receipts.
- `pako-cli` — the end-user `pako` command.
- `pako-build` — recipe parser and package publisher.
- `pako-oci` — OCI Distribution API client.
- `pako-trust` — TUF-backed release resolution.
- `pako-test-support` — deterministic fixtures and temporary XDG layouts.

## Built-in command documentation

Both command-line programs provide complete help at the root and command level:

```bash
pako --help
pako install --help
pako upgrade --help
pako verify --help
pako rollback --help
pako remove --help
pako list --help
pako status --help
pako recover --help

pako-build --help
pako-build lint --help
pako-build build --help
```

Each command documents its behavior, arguments, safety properties, side effects,
and representative examples. Unit tests walk the generated Clap command tree and
fail when a new command or argument is added without help text.

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
It uses the `file:` source form, whose path is resolved relative to `recipe.toml`, so
it neither downloads sources nor runs a build sandbox:

```bash
cargo run -p pako-build -- build examples/hello-local/recipe.toml \
  --target linux/x86_64 \
  --output /tmp/pako-hello-local
```

The resulting payload contains `bin/hello-pako`. Local source paths must remain
inside the recipe directory and are checked against the declared size and SHA-256.

## License

Licensed under either Apache License 2.0 or the MIT license, at your option.

## Code quality

The repository uses `rustfmt` with a 100-column limit and workspace-wide Clippy lints. Security-sensitive modules document their invariants and keep transaction phases in named functions rather than compressed statement chains. See [CONTRIBUTING.md](CONTRIBUTING.md).

## Validation status

The generated source tree has been checked for valid TOML, balanced Rust delimiters,
conflict markers, line length and compressed statement chains. A real Rust toolchain
was not available in the generation environment, so the first repository run must let
CI execute `cargo fmt`, `cargo check`, Clippy and the full test suite before the code is
considered release-ready.
