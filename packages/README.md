# Package catalog

The repository currently ships two development fixtures:

- `examples/hello-local` is a working hosted-package fixture used by the
  offline smoke flow.
- `examples/intellij-idea` is a working external-archive fixture for x86_64 and
  aarch64. Its loopback URL, digest, and size are intentionally test values and must not be
  published as a production package.

Before adding a real package release, maintainers must record:

- producer URL and every mirror;
- verified SHA-256, byte size, and archive format;
- license and upstream version;
- transforms and assertions;
- builder output and manifest diff;
- install, verify, upgrade, rollback, prune, and remove checks;
- desktop entry and launcher checks.

Never publish a recipe with placeholder checksums, unverified URLs, or missing
license information.
