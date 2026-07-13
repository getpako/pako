# Local Pako environment

The local environment runs an OCI registry on `127.0.0.1:5000` and serves the
signed TUF repository on `127.0.0.1:8080`. Registry data is stored in a
Compose-managed volume. All client data is isolated below `.dev/client`; the
normal user configuration and installed applications are not touched.

Requirements:

- Rust and Cargo matching the workspace toolchain;
- Docker with Compose, Podman with Compose, or the legacy `docker-compose` binary.

Run the complete end-to-end smoke test:

```bash
cargo xtask dev smoke
```

Useful commands:

```bash
cargo xtask dev up
cargo xtask dev publish examples/hello-local/recipe.toml
cargo xtask dev pako install hello-local
cargo xtask dev pako list
cargo xtask dev down
cargo xtask dev reset
```

`dev up` initializes the development TUF repository and client configuration when
needed. `dev reset` stops the containers, deletes `.dev`, and starts a completely
new environment with a new development trust root.

The generated TUF key is intentionally a single local development key. It must
never be reused for production publishing.
