# Contributing to kiss-tnc-bridge

## Building from source

Requires a Rust toolchain and `libdbus-1-dev` (Debian/Ubuntu) or `dbus-devel` (RHEL/Fedora):

```
$ sudo apt-get install libdbus-1-dev pkg-config   # Debian/Ubuntu
$ cargo build
$ cargo test
```

## Versioning

This project uses [Semantic Versioning](https://semver.org/). Applied to this project, it means:

- If we release a new version that won't work the same way using an older config file, we will
  increment the major version.
- New operating system platform support will increment the major version.
- New features are usually added in minor versions, but could be added in major versions too.
- Bug fixes are usually in patch versions.

## Creating a release

1. Update the `version` field in `Cargo.toml`:

   ```toml
   [package]
   version = "0.2.0"
   ```

2. Commit the version bump:

   ```
   git add Cargo.toml Cargo.lock
   git commit -m "Bump version to 0.2.0"
   ```

3. Tag the commit and push:

   ```
   git tag v0.2.0
   git push && git push --tags
   ```

The tag **must** start with `v` and the version after the `v` **must** match the
version in `Cargo.toml`. The CI workflow verifies this and will fail if they don't
match.

Pushing the tag triggers a GitHub Actions workflow that builds release artifacts:

- Generic Linux binaries (x86_64, arm64, and armhf) as `.tar.gz`
- Debian packages (`.deb`) for amd64, arm64, and armhf
- RPM packages (`.rpm`) for x86_64 and aarch64

All artifacts are attached to a GitHub Release which is created automatically.
