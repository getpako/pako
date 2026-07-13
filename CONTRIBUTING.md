# Contributing to Pako

Pako handles package installation and untrusted package metadata. Readability and explicit error handling are part of the security model.

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

## Review checklist

1. Is all external data validated before use?
2. Can an interrupted operation leave the active version unusable?
3. Does rollback preserve the previous installation?
4. Are temporary files published atomically?
5. Are tests included for malformed input and interrupted operations?
