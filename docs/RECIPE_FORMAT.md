# `recipe.toml` schema 1

A recipe is trusted build-side code. It is parsed and executed only by `pako-build`; the end-user
client never downloads or executes recipe scripts.

Each target declares its own sources, so `linux/x86_64` and `linux/aarch64` may use completely
different vendor archives. Source builds may provide `prepare`, `configure`, `build`, `check`, and
`install` Bash phases. These phases run in a rootless, read-only OCI build environment pinned by
digest. Network access is disabled by default.

The final output must be a self-contained relocatable tree. `pako-build` validates the tree, splits
regular files with `pako-fastcdc-v1`, writes immutable `PAKPACK1` files, and emits a canonical package
manifest and pack index.

See [`examples/intellij-idea/recipe.toml`](../examples/intellij-idea/recipe.toml) and
[`examples/source-build/recipe.toml`](../examples/source-build/recipe.toml).
