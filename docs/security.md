# Security model

Pako uses several independent checks. A package becomes active only after all
of them succeed:

1. TUF authenticates repository metadata and the release catalog.
2. The package manifest is validated against the resolved package and target.
3. The selected archive is checked against its SHA-256 digest and size.
4. Extraction validates paths, file types, modes, and symlink targets.
5. Every extracted entry is compared with the manifest.
6. The complete verified tree is checked against its tree digest before activation.

External archive URLs must use HTTPS; HTTP is accepted only for loopback test
fixtures. Credentials and URL fragments are rejected. The artifact cache is
keyed by SHA-256, checks size and digest on every hit, uses a per-digest lock,
and never treats partial files as cache entries.

## Trust boundaries

Recipe files and build scripts are maintainer-side inputs. They are never sent
to, interpreted by, or executed by the end-user client. Client installation does
not execute package-provided scripts.

The signed TUF catalog authenticates release selection, but it does not make an
application trustworthy by itself. Applications run with the user's normal
permissions and can access whatever the operating system grants them.

## Filesystem safety

Package paths must be relative, unique, sorted, and free of absolute, `.` or
`..` components. Symlinks must resolve lexically inside the package tree.
Extraction happens in a private staging directory and refuses to write through
symlink ancestors.

Installed trees are immutable after activation. Integrations use a global lock
and are only replaced when the existing file is owned by the same package and
matches its recorded receipt. Unmanaged user files and another package's
integrations are not silently overwritten.

## Limitations

Pako does not sandbox applications at runtime, protect external application
data, or provide content deduplication and delta updates. Retained versions
consume their full installed size until pruned. Recovery protects package state
from interrupted transactions, but it cannot make a malicious application safe
to execute.
