# Using Pako

Pako installs packages for the current user. A repository must be configured
before `install` or `upgrade` can resolve a package.

## Repository configuration

Pako reads repository configuration from `$XDG_CONFIG_HOME/pako/repository.json`
(`~/.config/pako/repository.json` by default). The file contains a trusted TUF
root and the metadata and target endpoints:

```json
{
  "name": "main",
  "root": "/path/to/trusted/root.json",
  "metadataUrl": "https://packages.example.org/metadata/",
  "targetsUrl": "https://packages.example.org/targets/"
}
```

Set `allowInsecureHttp` only for a local development server. External package
archives still require HTTPS.

## Common workflow

```bash
pako install vscodium
pako list
pako status vscodium
pako verify vscodium
pako upgrade vscodium
```

Mutating commands show a plan and ask for confirmation. Use `-y` or `--yes` in
automation. Read-only commands such as `list`, `status`, and `verify` do not
prompt.

## Install and upgrade

```bash
pako install PACKAGE [--channel CHANNEL]
pako install vscodium --channel beta
pako -y install vscodium

pako upgrade PACKAGE
pako upgrade PACKAGE --dry-run
pako upgrade PACKAGE --channel beta
```

The default channel is `stable`. Pako selects the host architecture and refuses
to install a release whose manifest does not match the resolved package and
target. An upgrade remembers the installed channel unless overridden.

`--dry-run` resolves metadata and displays the upgrade plan without changing the
installation.

## Inspect and verify

```bash
pako list
pako status
pako status PACKAGE
pako versions PACKAGE
pako verify PACKAGE
```

`status` and `list` read local receipts and do not contact the network. `verify`
hashes the active tree and checks paths, modes, sizes, symlinks, file digests,
and the final tree digest.

## Roll back, prune, and remove

```bash
pako rollback PACKAGE
pako rollback PACKAGE --to VERSION-RELEASE
pako -y prune PACKAGE --keep 2
pako remove PACKAGE
pako -y remove PACKAGE
```

Rollback activates an already retained version and does not download it. The
active version is always retained. Remove deletes package-owned integrations,
installed versions, manifests, receipts, and state; shared download cache data
may remain available for other operations.

## Recover interrupted work

```bash
pako recover
```

Install, upgrade, rollback, prune, and remove recover outstanding transaction
journals automatically before they begin. Run `recover` explicitly when
diagnosing an interrupted operation.
