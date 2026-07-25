# Building and publishing packages

`pako-build` is a maintainer-side tool. The normal workflow is:

```bash
pako-build lint packages/example/recipe.toml
pako-build build packages/example/recipe.toml \
  --target linux/x86_64 \
  --output build/example-x86_64
pako-build publish build/example-x86_64 \
  --reference registry.example.org/pako/example:1.0.0-1-linux-x86_64 \
  --tuf /srv/pako/tuf
```

## Lint and build

`lint` validates the recipe structure without downloading sources. `build`
prepares one target, verifies source checksums, applies transforms, runs any
configured build stages, and writes:

```text
package-manifest.json
payload.tar.zst
```

The generated manifest contains the complete file tree, integration metadata,
archive description, digest, and size. The archive is deterministic and is
validated again during publication.

## Publication

`publish` verifies the artifact directory, uploads the OCI package data, copies
the manifest and TUF archive into the configured TUF targets directory, updates
the signed `catalog.json`, and refreshes TUF metadata.

The catalog points to a manifest target such as
`manifests/example/1.0.0-1/linux-x86_64.json`. The manifest points to an archive
target such as `artifacts/example/1.0.0-1/linux-x86_64.tar.zst`.

The OCI tag is a publication convenience. Installation uses signed TUF target
names and immutable artifact digests.

Use `--insecure-http` only for a local development registry. Production
publication should use HTTPS and credentials supplied through the command
options or `PAKO_OCI_USERNAME` / `PAKO_OCI_PASSWORD`.

## Local TUF repository

Create a repository for development or CI experiments with:

```bash
pako-build tuf init /tmp/pako-tuf
```

This creates one local Ed25519 key. It is not a production signing layout and
must never be reused as a production trust root.
