# Local development

The local development environment provides an HTTP TUF server at
`127.0.0.1:8080`, isolated client state under `.dev/client`,
and a generated trust root under `.dev/tuf`.

Requirements are Rust/Cargo compatible with the workspace and Docker Compose,
Podman Compose, or the legacy `docker-compose` command.

## End-to-end smoke test

```bash
cargo xtask dev smoke
```

The smoke command recreates the isolated environment, builds the local example,
publishes it, installs it, verifies it, runs its launcher, and checks status.
Because it resets `.dev` and the Compose volume, it is intentionally a clean
integration test rather than a preservation workflow.

## Individual commands

```bash
cargo xtask dev up
cargo xtask dev publish examples/hello-local/recipe.toml
cargo xtask dev pako install hello-local
cargo xtask dev pako list
cargo xtask dev down
cargo xtask dev reset
```

`up` initializes missing TUF state and client configuration. `down` stops the
service. `reset` removes `.dev` and creates a new development trust root.

The local Compose definition and Nginx configuration live in [`dev/`](../dev/).
