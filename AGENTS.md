# Agent instructions for Pako

These instructions apply to the whole repository. More specific `AGENTS.md`
files in subdirectories, if added later, may refine them for that subtree.

## Before changing code

1. Read the relevant module and its tests before editing.
2. Check `git status --short` and preserve unrelated user changes.
3. Identify the user-visible behavior, configuration, schema, workflow, or
   security boundary affected by the change.
4. Search `docs/`, `README.md`, `CONTRIBUTING.md`, command help text, and nearby
   examples for descriptions of that behavior.

## Documentation synchronization rule

Documentation is part of the implementation. Whenever a code change modifies
behavior that is described or expected by documentation, update the affected
documentation in the same change.

This includes changes to:

- CLI commands, options, output, prompts, exit behavior, or examples;
- recipe fields, validation rules, generated manifests, or build workflows;
- repository configuration, TUF metadata, artifact formats, or publishing;
- installation, upgrade, rollback, recovery, integration, or filesystem layout;
- security guarantees, trust boundaries, supported platforms, or limitations;
- local development commands, smoke tests, services, or reset semantics.

Use the documentation map below to find the likely file. If the behavior is
not documented yet, add the smallest useful section in the appropriate guide.
Do not leave documentation claims that contradict the implementation.

## Documentation map

| Change area | Primary documentation |
| --- | --- |
| System design and data flow | `docs/Pako.md` |
| End-user commands and repository setup | `docs/commands.md` |
| Recipe schema and package inputs | `docs/RECIPE_FORMAT.md` |
| Build and publication workflow | `docs/publishing.md` |
| Local services and smoke tests | `docs/development.md`, `dev/README.md` |
| Trust, validation, and limitations | `docs/security.md` |
| Documentation navigation | `docs/README.md`, `README.md` |
| Contributor workflow | `CONTRIBUTING.md` |

When changing command help, keep the Rust help text and the corresponding guide
consistent. The generated `--help` output is authoritative for exact option
names and defaults; prose guides should explain workflows and safety behavior.

## Implementation rules

- Keep functions focused and preserve explicit error handling.
- Validate all external data before using it.
- Validate paths before joining them to managed roots.
- Never execute recipe scripts on an end-user machine.
- Preserve transaction safety: an interrupted operation must leave either the
  old complete state or the new complete state.
- Do not use `unwrap()` or `expect()` for data controlled by packages,
  registries, users, or the filesystem.
- Add tests for malformed input, changed behavior, and interrupted operations
  when relevant.

## Verification and handoff

Run the narrowest relevant checks while iterating, then run the full applicable
checks before handoff:

```bash
cargo fmt --all --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

For documentation-only changes, at minimum check links and formatting manually,
run `git diff --check`, and verify that examples match the current CLI help.

Before finishing, review:

1. `git diff --check` output;
2. the final `git status --short`;
3. whether behavior changes have matching documentation changes;
4. whether tests and command examples still describe the same workflow.
