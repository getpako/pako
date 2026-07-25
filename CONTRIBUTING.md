# Contributing to Pako

Pako handles package installation and untrusted package metadata. Readability and explicit error handling are part of the security model.

Repository-level agent guidance is in [`AGENTS.md`](AGENTS.md). Contributors and
coding agents should treat documentation as part of the implementation: when a
change affects documented behavior, update the relevant guide in the same
change.

## Code rules

- Run `cargo fmt --all` before every commit.
- Run `cargo clippy --workspace --all-targets -- -D warnings`.
- Run `cargo test --workspace`.
- Keep functions focused. Extract phases of multi-step transactions into named helpers.
- Do not place multiple statements or function definitions on one line.
- Add comments for invariants, security boundaries and non-obvious recovery behavior. Do not comment obvious syntax.
- Do not use `unwrap()` or `expect()` on data controlled by packages, registries or the filesystem.
- Validate paths before joining them to a managed root.
- Never execute recipe scripts on an end-user machine.

## Documentation maintenance

Before submitting a behavior change, search the documentation and command help
for the affected behavior. Update documentation when changing:

- CLI commands, options, output, prompts, or examples;
- recipe fields, validation, build, or publication behavior;
- repository configuration, TUF metadata, or artifact formats;
- installation, upgrade, rollback, recovery, integrations, or filesystem layout;
- security guarantees, supported platforms, or local development workflows.

Use the [documentation index](docs/README.md) and the documentation map in
[`AGENTS.md`](AGENTS.md). If the behavior is not documented, add a focused
section rather than expanding an unrelated page. Keep command help and prose
guides consistent.

## Review checklist

1. Is all external data validated before use?
2. Can an interrupted operation leave the active version unusable?
3. Does rollback preserve the previous installation?
4. Are temporary files published atomically?
5. Are tests included for malformed input and interrupted operations?
6. Does the change require a documentation update, and is it included?
7. Do documentation examples still match the current command help and behavior?
