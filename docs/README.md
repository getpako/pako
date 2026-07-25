# Pako documentation

Pako is a user-space Linux package manager for self-contained applications.
Start with the guide that matches the task:

| Goal | Guide |
| --- | --- |
| Understand the system | [Architecture](Pako.md) |
| Install or manage packages | [Using Pako](commands.md) |
| Write a package recipe | [Recipe format](recipe.md) |
| Build and publish a package | [Publishing](publishing.md) |
| Run local services or the smoke test | [Local development](development.md) |
| Review trust and safety properties | [Security model](security.md) |
| Contribute code | [Contributing](../CONTRIBUTING.md) |

## Documentation conventions

- Commands assume the binaries are on `PATH`.
- Examples use `linux/x86_64`; replace it with `linux/aarch64` when appropriate.
- `pako` is the end-user client. `pako-build` is a maintainer-side tool.
- Recipe scripts run during package creation only; the end-user client never
  executes them.

The command-line programs also provide detailed, version-matched help:

```bash
pako --help
pako install --help
pako upgrade --help
pako-build --help
pako-build build --help
```
