# Building and publishing packages

`pako-build` is a maintainer-side tool. The normal workflow is:

```bash
pako-build lint packages/example/recipe.toml
pako-build build packages/example/recipe.toml \
  --target linux/x86_64 \
  --output build/example-x86_64
pako-build publish build/example-x86_64 \
  --tuf /srv/pako/tuf
```

## Lint and build

`lint` validates the recipe structure without downloading sources. `build`
prepares one target, verifies source checksums, applies transforms, runs any
configured build stages, and writes:

```text
package-manifest.json
package.tar.zst (hosted targets only)
```

The generated manifest contains the complete file tree, integration metadata,
archive description, digest, and size. The archive is deterministic and is
validated again during publication.

## Publication

`publish` verifies the manifest, copies it into the configured TUF targets
directory, copies `package.tar.zst` for hosted packages, updates the signed
`catalog.json`, and refreshes TUF metadata. External upstream archives are not
downloaded or copied during publication.

The catalog points to a manifest target such as
`manifests/example/1.0.0-1/linux-x86_64.json`. The manifest points to an archive
target such as `artifacts/example/1.0.0-1/linux-x86_64.tar.zst`.

External builds publish only the manifest. Hosted builds publish the manifest
and `package.tar.zst`; the upstream archive is never copied to TUF.

Installation uses signed TUF target names and immutable artifact digests.

## Local TUF repository

Create a repository for development or CI experiments with:

```bash
pako-build tuf init /tmp/pako-tuf
```

This creates one local Ed25519 key. It is not a production signing layout and
must never be reused as a production trust root.
