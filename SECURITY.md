# Security Policy

Pako is beta software. The supported security line is the latest `0.1.x`
release; older releases may not receive fixes.

Please report suspected vulnerabilities privately through GitHub Private
Vulnerability Reporting for this repository. Repository maintainers must enable
that feature in the repository security settings. Do not open a public issue
for an unpatched vulnerability.

Pako does not sandbox installed applications at runtime. Applications run with
the user's normal operating-system permissions. External archives are fully
downloaded and verified by digest and size before safe extraction.
