# Releasing Pako

1. Update the workspace version and `CHANGELOG.md`.
2. Run `cargo fmt --all --check`, check, clippy, and tests.
3. Build and inspect `pako`, `pako-build`, and the package fixtures locally.
4. Verify the production TUF root, role keys, expiry periods, and target set.
5. Run `pako-build tuf refresh /srv/pako/tuf` when metadata renewal is due.
6. Create and push a version tag such as `v0.1.0-beta.1`.
7. Review the release workflow output and generated `SHA256SUMS`.
8. Test installation and verification from the release artifacts on a clean
   system with a fresh XDG data, state, config, and cache directory.

Production TUF keys are not stored in this repository or in CI. Keep the root
key offline, use separate protected role keys for targets, snapshot, and
timestamp, maintain encrypted backups, and document rotation or recovery
before publishing.
