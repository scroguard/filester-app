# Filester for Linux

A native GTK4 / libadwaita desktop client for the [Filester](https://filester.me)
cloud storage service, written in Rust.

> **Unofficial.** This project is not affiliated with, endorsed by, or supported
> by Filester. It talks to the public Filester HTTP API using a key you supply.

[![CI](https://github.com/scroguard/filester-app/actions/workflows/ci.yml/badge.svg)](https://github.com/scroguard/filester-app/actions/workflows/ci.yml)
License: GPL-3.0-or-later

---

## Project status

| Phase | Scope | State |
|-------|-------|-------|
| 1 | Repository layout, build configuration, CI, desktop metadata, application shell | **Done** |
| 2 | API engine: async HTTP, token storage, rate limiting/backoff, streamed multipart uploads, folder-tree reconstruction, unit tests | Planned |
| 3 | Main window: sidebar navigation, file/folder grid, transfer progress, event handling | Planned |

Sections below that describe Phase 2/3 behaviour are labelled as such. They
are design commitments, not shipped features, until that phase lands.

## Why Rust + gtk4-rs

- **Memory safety without a GC.** The crate is compiled with
  `#![forbid(unsafe_code)]` (via `[lints.rust]` in `Cargo.toml`). The only
  `unsafe` lives in audited upstream bindings (gtk-rs, rustls/aws-lc).
- **Predictable resource use for 10 GB uploads.** Files are streamed in bounded
  chunks through `tokio`, `reqwest` and `multipart`, with no interpreter heap
  holding buffers.
- **Clean dependency mapping across distros.** The only system libraries are
  GTK 4 and libadwaita. TLS is handled by rustls, so there is no OpenSSL ABI
  coupling. D-Bus (Secret Service) is handled by pure-Rust `zbus`, so there is
  no libdbus or libsecret dependency.
- **First-class GNOME integration.** libadwaita gives adaptive layouts, the
  system dark/light preference, and HIG-conformant widgets.

## Repository layout

```
filester-app/
├── .github/
│   ├── dependabot.yml            # weekly cargo + Actions update PRs
│   └── workflows/ci.yml          # fmt, clippy, 4-distro build/test, MSRV, metadata, cargo-deny
├── data/
│   ├── icons/hicolor/scalable/apps/io.github.scroguard.Filester.svg
│   ├── io.github.scroguard.Filester.desktop
│   └── io.github.scroguard.Filester.metainfo.xml
├── src/
│   ├── main.rs                   # entry point: logging + GApplication
│   ├── config.rs                 # app ID and other compile-time identity
│   ├── api/                      # Phase 2 — no GTK imports allowed here
│   │   ├── mod.rs                #   public engine facade
│   │   ├── client.rs             #   reqwest client, auth header, HTTPS enforcement
│   │   ├── rate_limit.rs         #   X-RateLimit-* tracking, 429 + Retry-After backoff
│   │   ├── upload.rs             #   streamed multipart uploads with progress
│   │   ├── models.rs             #   serde DTOs for API responses
│   │   ├── tree.rs               #   folder hierarchy from parent-ID links
│   │   └── error.rs              #   typed error enum
│   ├── secrets.rs                # Phase 2 — Secret Service / portal token store
│   ├── worker.rs                 # Phase 2 — tokio runtime ↔ GTK bridge (async-channel)
│   └── ui/                       # Phase 3
│       ├── mod.rs
│       ├── window.rs             #   AdwApplicationWindow + NavigationSplitView
│       ├── sidebar.rs            #   folder tree navigation
│       ├── file_grid.rs          #   GridView/ColumnView listing
│       └── transfers.rs          #   progress rows for uploads
├── tests/                        # Phase 2 — integration tests against a wiremock server
├── Cargo.toml                    # dependencies, lint policy, release profile
├── Cargo.lock                    # committed: pinned, auditable dependency graph
├── clippy.toml / rustfmt.toml / rust-toolchain.toml
├── deny.toml                     # supply-chain policy (advisories, licences, sources)
├── .editorconfig / .gitattributes / .gitignore
├── LICENSE                       # GPL-3.0
├── SECURITY.md                   # vulnerability reporting policy
└── README.md
```

## Supported platforms

| Distribution | Minimum release | GTK | libadwaita | Notes |
|--------------|-----------------|-----|------------|-------|
| Arch Linux / CachyOS / EndeavourOS / Manjaro | rolling | ≥ 4.14 | ≥ 1.5 | CI: `archlinux:base-devel` |
| Fedora | 40 | ≥ 4.14 | ≥ 1.5 | CI: `fedora:latest` |
| RHEL / AlmaLinux / Rocky | 10 | 4.16 | 1.6 | `-devel` packages may require the CRB repository |
| Debian | 13 (trixie) | 4.18 | 1.7 | CI: `debian:trixie` |
| Ubuntu / Linux Mint | 24.04 / 22 | 4.14 | 1.5 | CI: `ubuntu:24.04` (oldest supported baseline) |

Debian 12, Ubuntu 22.04 and RHEL 9 ship libadwaita versions older than 1.5 (or
none at all) and cannot build natively. A Flatpak build is planned for them
(see [Flatpak](#flatpak-planned)).

The floors are set by the `v4_14` / `v1_5` feature flags on the `gtk` and `adw`
dependencies in `Cargo.toml`. Raising them drops Ubuntu 24.04 / Mint 22.

## Dependency matrix

### Build dependencies

| Purpose | Arch / CachyOS (`pacman`) | Debian / Ubuntu / Mint (`apt`) | Fedora / RHEL (`dnf`) |
|---------|---------------------------|--------------------------------|-----------------------|
| C/C++ toolchain (for `aws-lc-sys`, GTK `-sys` crates) | `base-devel` | `build-essential` | `gcc gcc-c++ make` |
| pkg-config | `pkgconf` | `pkg-config` | `pkgconf-pkg-config` |
| GTK 4 headers | `gtk4` | `libgtk-4-dev` | `gtk4-devel` |
| libadwaita headers | `libadwaita` | `libadwaita-1-dev` | `libadwaita-devel` |
| Rust toolchain manager | `rustup` | `rustup` | `rustup` (then run `rustup-init`) |
| Git | `git` | `git` | `git` |

Not required: OpenSSL headers, libdbus, libsecret. The build intentionally
avoids them, and `deny.toml` bans `openssl-sys` and `native-tls`.

### Runtime dependencies

| Purpose | Arch / CachyOS | Debian / Ubuntu / Mint | Fedora / RHEL |
|---------|----------------|------------------------|---------------|
| GTK 4 | `gtk4` | `libgtk-4-1` | `gtk4` |
| libadwaita | `libadwaita` | `libadwaita-1-0` | `libadwaita` |
| Secret Service provider (any one) | `gnome-keyring` / `kwallet` / `keepassxc` | `gnome-keyring` / `kwalletmanager` / `keepassxc` | `gnome-keyring` / `kf6-kwallet` / `keepassxc` |
| CA certificates | `ca-certificates` | `ca-certificates` | `ca-certificates` |

GNOME and KDE Plasma desktops already run a Secret Service provider. On
minimal window managers (Sway, Hyprland, i3), start `gnome-keyring-daemon`
or enable KeePassXC's Secret Service integration.

### One-line setup

```sh
# Arch Linux / CachyOS
sudo pacman -S --needed base-devel pkgconf gtk4 libadwaita rustup git

# Debian 13 / Ubuntu 24.04+ / Linux Mint 22+
sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev rustup git

# Fedora 40+ / RHEL 10 (enable CRB first on RHEL clones)
sudo dnf install gcc gcc-c++ make pkgconf-pkg-config gtk4-devel libadwaita-devel rustup git
rustup-init -y --profile minimal   # Fedora only: its rustup package ships the installer
```

Then, on every distribution:

```sh
rustup default stable
```

`rust-toolchain.toml` pins the channel and pulls in `clippy` and `rustfmt`
automatically on first use. The minimum supported Rust version is **1.92**
(required by gtk4-rs 0.11 and oo7 0.6). Distribution-packaged `rustc` is
usually older than that, so use rustup.

## Building and running

```sh
git clone git@github.com:scroguard/filester-app.git
cd filester-app
cargo run                       # debug build
cargo build --release --locked  # optimised binary in target/release/filester-app
```

Logging goes to stderr and is controlled by `FILESTER_LOG` (tracing
`EnvFilter` syntax):

```sh
FILESTER_LOG=filester_app=debug cargo run
```

### Installing (per user, no root)

```sh
cargo build --release --locked
install -Dm755 target/release/filester-app ~/.local/bin/filester-app
install -Dm644 data/io.github.scroguard.Filester.desktop \
    ~/.local/share/applications/io.github.scroguard.Filester.desktop
install -Dm644 data/io.github.scroguard.Filester.metainfo.xml \
    ~/.local/share/metainfo/io.github.scroguard.Filester.metainfo.xml
install -Dm644 data/icons/hicolor/scalable/apps/io.github.scroguard.Filester.svg \
    ~/.local/share/icons/hicolor/scalable/apps/io.github.scroguard.Filester.svg
```

For a system-wide install, replace `~/.local` with `/usr/local` and run as
root. Make sure `~/.local/bin` is on your `PATH`, because the desktop entry
calls `filester-app` by name.

## Development

```sh
cargo fmt --all                                    # format
cargo clippy --locked --all-targets -- -D warnings # lint (pedantic, see Cargo.toml)
cargo test --locked                                # unit + integration tests
cargo deny check                                   # supply-chain audit (cargo install --locked cargo-deny)
desktop-file-validate data/*.desktop
appstreamcli validate --no-net data/*.metainfo.xml
```

CI (`.github/workflows/ci.yml`) runs all of the above. It builds and tests
inside Arch, Fedora, Debian 13 and Ubuntu 24.04 containers, using each distro's
own package names from the matrix above, and checks the MSRV separately.

## Security & Auditing

This section is the audit map. Each item names the mechanism and where it
lives. Items marked *(Phase 2)* or *(Phase 3)* are design requirements for code
that is not yet merged. Auditors should treat them as claims to verify once
that phase lands.

### Threat model

| Asset | Threat | Mitigation |
|-------|--------|------------|
| Filester API key | Disclosure via disk, logs, process list, crash dumps, VCS | Secret Service storage, `SecretString`, log redaction, no argv/env use, `.gitignore` guards |
| API key in transit | Interception or downgrade | HTTPS-only base URL, rustls with platform verifier, no cleartext fallback |
| User files | Exfiltration to the wrong host | Single fixed origin, cross-origin redirects refused |
| Local system | Memory corruption, resource exhaustion | `forbid(unsafe_code)`, streamed uploads, bounded channels |
| Build pipeline | Dependency or CI compromise | `Cargo.lock`, `cargo-deny`, SHA-pinned actions, read-only CI token |

### Token handling

- **At rest** *(Phase 2)*: The API key is stored only in the desktop Secret
  Service (GNOME Keyring, KWallet, KeePassXC) through
  [`oo7`](https://crates.io/crates/oo7), with attributes namespaced by the app
  ID. Inside a Flatpak sandbox, oo7 automatically uses the XDG Secret portal
  instead. The key is never written to a config file, cache, or GSettings.
- **In memory** *(Phase 2)*: The key is held in `secrecy::SecretString`, which
  zeroizes its buffer on drop and prints `[REDACTED]` from `Debug`. It is
  exposed only at the moment the `Authorization` header value is built. That
  header value is marked sensitive (`HeaderValue::set_sensitive`) so hyper and
  tracing do not render it.
- **Never via the process environment or argv**: There is no `--api-key` flag
  and no `FILESTER_API_KEY` variable, because both leak through
  `/proc/<pid>/cmdline`, `/proc/<pid>/environ` and shell history. The key is
  entered in the GUI only *(Phase 3)*.
- **Logging policy**: No log statement may format request headers, the token,
  or full response bodies. Logged URLs contain only the path, never query
  strings. A unit test asserts that the `Debug` output of the client and
  credential types contains no secret material *(Phase 2)*.
- **Sign-out** *(Phase 3)*: Removes the keyring item and drops every in-memory
  copy.

### Transport security

- The base URL is the compile-time constant `https://filester.me`. The client
  refuses any non-`https` URL before a request is built *(Phase 2)*.
- TLS uses **rustls** (memory-safe), with certificate verification delegated
  to `rustls-platform-verifier`, which reads the distro's trust store. This
  respects enterprise CAs installed via `update-ca-trust` or
  `update-ca-certificates`. There is no option to disable verification.
- The redirect policy allows **same-origin redirects only**. A redirect to
  another host is treated as an error rather than followed, so the
  `Authorization` header can never be forwarded off-origin *(Phase 2)*.
- Every request has explicit connect and read timeouts. Uploads use an idle
  timeout instead of a total timeout, so slow 10 GB transfers are not killed
  mid-stream *(Phase 2)*.

### Rate limiting and abuse resistance *(Phase 2)*

- `X-RateLimit-Limit`, `X-RateLimit-Remaining` and `X-RateLimit-Reset` are
  parsed on every response. Requests are delayed proactively when the
  remaining budget hits zero.
- HTTP 429 triggers exponential backoff with full jitter. A `Retry-After`
  header (delta-seconds or HTTP-date) always takes precedence and acts as a
  lower bound. Retries are capped in count and in total wall-clock time, so a
  misbehaving server cannot stall the app indefinitely.
- Non-idempotent requests (uploads) are retried only when the server
  explicitly answered 429, meaning the request was rejected before
  processing.

### Data isolation

- File contents are never cached locally by the app. Uploads are read
  straight from the user-selected file and streamed in fixed-size chunks.
  Peak memory is independent of file size.
- Folder and file metadata lives in process memory only and is discarded on
  exit.
- The app writes no files outside `$XDG_CONFIG_HOME/filester-app/` (UI
  preferences only, no secrets). The only exception is download targets the
  user explicitly chooses in a file dialog *(Phase 3)*.
- Server-supplied names are treated as untrusted. They are rendered as plain
  text (never Pango markup), and they are never used to build local paths
  without sanitising path separators, `..` and NUL bytes *(Phase 3)*.

### Concurrency model

- The GTK main thread never performs I/O. A dedicated multi-threaded tokio
  runtime runs all networking, file reading and tree reconstruction. Results
  return to the UI over `async-channel` and are consumed with
  `glib::spawn_future_local` *(Phase 2/3)*.
- Channels between the worker and the UI are bounded, so a fast producer
  cannot grow memory without limit.

### Code and supply chain

- `unsafe_code = "forbid"` crate-wide. Clippy runs at `pedantic` level in CI
  with `-D warnings`, and `unwrap_used`, `panic`, `todo`, `unimplemented` and
  `dbg_macro` are denied in non-test code.
- Release builds keep `overflow-checks = true`.
- `Cargo.lock` is committed, and every CI cargo command uses `--locked`.
- `cargo-deny` (`deny.toml`) enforces RustSec advisories (including yanked and
  unmaintained crates), a GPL-compatible licence allow-list, crates.io as the
  only source, no wildcard versions, and a ban on `openssl-sys` and
  `native-tls`.
- GitHub Actions are pinned to full commit SHAs. The workflow token is
  `contents: read`, and `actions/checkout` runs with
  `persist-credentials: false`. Dependabot proposes updates for both cargo and
  Actions.
- `.gitignore` excludes build trees, packaging output, HTTP captures (`*.har`,
  `*.pcap`, which can contain bearer tokens), `.env` files and common
  key/credential file patterns. Always review `git status` before pushing.

### Reporting a vulnerability

See [SECURITY.md](SECURITY.md).

## Flatpak (planned)

The layout is already Flatpak-ready: the reverse-DNS app ID is used
consistently for the binary metadata, desktop file, AppStream file and icon.
A future manifest (`build-aux/flatpak/io.github.scroguard.Filester.json`)
will target `org.gnome.Platform` with these sandbox permissions:

- `--share=network`, `--share=ipc`, `--socket=wayland`, `--socket=fallback-x11`,
  `--device=dri`
- **No** `--filesystem=` grants. Files are opened and saved through the
  FileChooser portal.
- **No** `--talk-name=org.freedesktop.secrets`. oo7 uses the Secret portal.

Offline Cargo sources will be generated from `Cargo.lock` with
`flatpak-cargo-generator`.

## License

Copyright © 2026 scroguard.

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU General Public License as published by the Free Software
Foundation, either version 3 of the License, or (at your option) any later
version. See [LICENSE](LICENSE).
