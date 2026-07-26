# IntelliJ IDEA external fixture

This is a working schema and build fixture for `external-archive`. It is not a
production IntelliJ package: the loopback URL points at the checked-in test
archive. Replace it only after independently verifying a real upstream
release, license, size, checksum, and transforms.

Serve the fixture and build it with:

```bash
python3 -m http.server 8765 --directory examples/intellij-idea
pako-build lint examples/intellij-idea/recipe.toml
pako-build build examples/intellij-idea/recipe.toml \
  --target linux/x86_64 --output build/intellij-idea
```
