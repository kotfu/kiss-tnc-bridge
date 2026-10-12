# Contributing to kiss-tnc-bridge

## Building from source

On Linux, building requires a Rust toolchain and `libdbus-1-dev` (Debian/Ubuntu)
or `dbus-devel` (RHEL/Fedora):

```
$ sudo apt-get install libdbus-1-dev pkg-config   # Debian/Ubuntu
$ cargo build
$ cargo test
```

On macOS, `cargo build` and `cargo test` work with just a Rust toolchain. The
Bluetooth code (BlueZ, through the `bluer` crate) is Linux-only and isn't
compiled there.

## Development setup

kiss-tnc-bridge runs on Linux, but most of it can be developed anywhere. The
workflow has three tiers, from fastest to slowest.

### 1. Native builds and tests (macOS or Linux)

Everything except the Bluetooth hardware layer builds and tests natively:
KISS framing, config parsing and validation, bridging logic, and TCP. Use
plain `cargo test` and `cargo run` for these. This is where most work happens.

### 2. Linux builds with `cross` (from macOS or another architecture)

[cross](https://github.com/cross-rs/cross) builds inside a Linux container,
using the same setup as the release workflow (see `Cross.toml`). It needs a
running Docker engine (Docker Desktop, OrbStack, or similar).

```
$ cargo install cross
$ cross clippy --target aarch64-unknown-linux-gnu    # lint the Linux-only code
$ cross test --target aarch64-unknown-linux-gnu      # run the tests on Linux
$ cross build --target aarch64-unknown-linux-gnu     # a binary for a 64-bit Pi
```

Use `arm-unknown-linux-gnueabihf` instead for 32-bit Raspberry Pi OS.

cross's build images are x86_64 Linux, so on an Apple Silicon Mac they run
under emulation. That works, but it's slower than a native build; in Docker
Desktop, turning on "Use Rosetta for x86_64/amd64 emulation" helps. The first
build also takes a while because it builds the image. `cross test` for an ARM
target runs the test binaries under QEMU inside that container.

Run `cross clippy` and `cross test` before sending changes that touch
Linux-only code: it's the quickest way to catch code that builds on macOS but
not on Linux.

### 3. Hardware tests on a Raspberry Pi

Anything involving Bluetooth (BlueZ, radios, phones) has to be tested on real
hardware. A Pi works well as a dedicated development box. Rather than building
on the Pi, which is slow and lets its toolchain drift from what releases use,
cross-compile on your workstation and copy the binary over with
`scripts/pi-deploy.sh`. It:

1. cross-builds for the Pi,
2. copies the binary (and optionally a test config) to the Pi with `rsync`,
3. stops the installed `kiss-tnc-bridge` service, since only one instance can
   hold the Bluetooth adapter,
4. runs your build in the foreground with its log streaming back to your
   terminal,
5. starts the installed service again when you press Ctrl-C.

Setup:

- The Pi needs BlueZ and SSH access with keys from your workstation. Having
  kiss-tnc-bridge installed as a service is optional; the script restarts it
  only if it was running.
- Create `scripts/pi-deploy.local` (git ignores it) with your Pi's details:

  ```
  PI_HOST=raspberrypi.local
  PI_USER=pi
  PI_TARGET=aarch64-unknown-linux-gnu
  # PI_CONFIG=path/to/test.conf   # optional: a config to copy and use
  ```

Then:

```
$ scripts/pi-deploy.sh                 # build, copy, run with -d
$ scripts/pi-deploy.sh -- -dd          # pass other arguments to kiss-tnc-bridge
$ scripts/pi-deploy.sh --release       # release build
$ scripts/pi-deploy.sh --copy-only     # build and copy, but don't run
$ scripts/pi-deploy.sh --help
```

Running the build on the Pi needs root, so `sudo` on the Pi may ask for your
password, and again when restarting the service if a long run outlasts sudo's
timeout. Run the script from a terminal so those prompts reach you. Treat the Pi as a test machine: the script stops and starts its
kiss-tnc-bridge service freely.

## Versioning

This project uses [Semantic Versioning](https://semver.org/). Applied to this project, it means:

- If we release a new version that won't work the same way using an older config file, we will
  increment the major version.
- New operating system platform support will increment the major version.
- New features are usually added in minor versions, but could be added in major versions too.
- Bug fixes are usually in patch versions.

## Creating a release

> **No releases while 1.0 is in progress.** Once the 1.0 work in
> [docs/plan-1.0.md](docs/plan-1.0.md) starts landing on `main`, `main` can't
> be released until 1.0 is done. Fixes for 0.5 users go on a `0.5.x` branch;
> see the plan for details.

Releases are created using [cargo-release](https://github.com/crate-ci/cargo-release).
Install it once with:

```
$ cargo install cargo-release
```

Before creating a release:

1. Update `CHANGELOG.md` with the changes for the new version.
2. Commit the changelog update.
3. Run `cargo release` with the appropriate bump level:

```
$ cargo release patch   # 0.1.0 → 0.1.1
$ cargo release minor   # 0.1.0 → 0.2.0
$ cargo release major   # 0.1.0 → 1.0.0
```

This will bump the version in `Cargo.toml`, commit, tag with a `v` prefix (e.g.,
`v0.2.0`), and push. Add `--execute` to actually perform the release — without it,
`cargo release` does a dry run.

The tag **must** start with `v` and the version after the `v` **must** match the
version in `Cargo.toml`. The CI workflow verifies this and will fail if they don't
match.

Pushing the tag triggers a GitHub Actions workflow that builds release artifacts:

- Generic Linux binaries (x86_64, arm64, and armhf) as `.tar.gz`
- Debian packages (`.deb`) for amd64, arm64, and armhf
- RPM packages (`.rpm`) for x86_64 and aarch64

All artifacts are attached to a GitHub Release which is created automatically.
