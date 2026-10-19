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
| 2 | API engine: async HTTP, token storage, rate limiting/backoff, streamed multipart uploads, folder-tree reconstruction, unit tests | **Done** |
| 3 | Main window: sidebar navigation, file/folder grid, transfer progress, event handling | **Done** |

## Features

- Sign in with an API key. The key is validated against the API, then
  stored in the system keyring, so later launches sign in automatically.
- Sidebar folder tree, rebuilt from parent links. It repairs orphaned or
  cyclic folders instead of hiding them.
- Icon grid of subfolders and files. Double-click a folder to open it, or a
  file to open its share page in the browser. Breadcrumbs appear in the
  header.
- Uploads from the Upload button or by drag-and-drop. Two run at a time, the
  rest queue, and each has a progress bar and a cancel button. Closing the
  window with uploads in progress asks first.
- New folder (private by default), delete with confirmation, copy share link.
- Storage usage gauge.
- Adaptive layout that collapses to a single pane on narrow windows.

### Keyboard shortcuts

| Shortcut | Action |
|----------|--------|
| <kbd>Ctrl</kbd>+<kbd>U</kbd> | Upload files |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>N</kbd> | New folder |
| <kbd>F5</kbd> / <kbd>Ctrl</kbd>+<kbd>R</kbd> | Refresh |
| <kbd>Alt</kbd>+<kbd>↑</kbd> | Parent folder |
| <kbd>Enter</kbd> | Open selected item |
| <kbd>Delete</kbd> | Delete selected item (file grid focused) |
| <kbd>Ctrl</kbd>+<kbd>C</kbd> | Copy share link (file grid focused) |
| <kbd>Ctrl</kbd>+<kbd>W</kbd> / <kbd>Ctrl</kbd>+<kbd>Q</kbd> | Close / quit |

Delete and Ctrl+C are bound only to the file grid, so they can never fire
while you're typing in a text field.

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
│   ├── main.rs                   # binary: logging + GApplication (all GTK code)
│   ├── lib.rs                    # library root: everything testable headlessly
│   ├── config.rs                 # app ID, API origin, User-Agent
│   ├── api/                      # REST engine — no GTK imports allowed here
│   │   ├── mod.rs                #   public facade and re-exports
│   │   ├── auth.rs               #   ApiKey: validation, redaction, sensitive header
│   │   ├── client.rs             #   FilesterClient: endpoints, retries, redirects, size caps
│   │   ├── rate_limit.rs         #   X-RateLimit-* tracking, 429 + Retry-After backoff
│   │   ├── upload.rs             #   streamed multipart body, progress, stall watchdog
│   │   ├── models.rs             #   serde wire types (lenient decoding)
│   │   ├── tree.rs               #   folder hierarchy from parent-ID links, anomaly repair
│   │   └── error.rs              #   ApiError, server-text sanitising
│   ├── secrets.rs                # Secret Service / portal token store (oo7)
│   ├── worker.rs                 # tokio runtime ↔ GTK bridge (oneshot + async-channel)
│   └── ui/                       # GTK front end (binary only)
│       ├── mod.rs                #   Location, spawn_ui (worker → main-thread bridge)
│       ├── window.rs             #   window, sign-in, navigation, actions, dialogs
│       ├── sidebar.rs            #   TreeListModel folder tree + storage gauge
│       ├── file_grid.rs          #   GridView tiles + context menu
│       ├── transfers.rs          #   upload queue with progress bars
│       └── format.rs             #   error text, share-URL validation, markup safety
├── tests/api_client.rs           # end-to-end client tests against a wiremock server
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
or enable KeePassXC's Secret Service integration. Without one, the app still
works: keyring calls give up after 30 seconds, and you sign in each launch.

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

## Filester API coverage

The engine talks to the REST API documented at
<https://filester.me/api-docs>. Note that the API host is
**`https://u1.filester.me`**: the main site `filester.me` answers 404 on
every `/api/v1/*` path.

| Endpoint | `FilesterClient` method | Retried on |
|----------|-------------------------|------------|
| `GET /health` | `health` (no auth) | 429, 502/503/504, timeout, connect |
| `GET /api/v1/account` | `account` | same |
| `GET /api/v1/folders` | `list_folders`, `folder_tree` | same |
| `GET /api/v1/folder/{id}/files` | `folder_files` | same |
| `GET /api/v1/files` | `list_files` | same |
| `GET /api/v1/file/{id}` | `file` | same |
| `GET /api/v1/upload/status` | `upload_status` | same |
| `POST /api/v1/upload` | `upload` (guest or authenticated) | 429, connect only |
| `POST /api/v1/folder` | `create_folder` | 429, connect only |
| `POST /api/v1/folder/thumbnail` | `set_folder_thumbnail` | 429, connect only |
| `POST /api/v1/folder/move`, `/folders/move` | `move_folder`, `move_folders` | 429, connect only |
| `POST /api/v1/file/move`, `/files/move` | `move_file`, `move_files` | 429, connect only |
| `POST /file/delete`, `/folder/delete` | `delete_files`, `delete_folders` | 429, connect only |

POST requests are never retried after the server may have processed them.
A 429 means the request was rejected up front, and a connect failure means
it never left the machine.

## Development

```sh
cargo fmt --all                                    # format
cargo clippy --locked --all-targets -- -D warnings # lint (pedantic, see Cargo.toml)
cargo test --locked                                # unit + integration tests
cargo test --test keyring_live -- --ignored --nocapture  # read-only check against your real keyring
cargo deny check                                   # supply-chain audit (cargo install --locked cargo-deny)
desktop-file-validate data/*.desktop
appstreamcli validate --no-net data/*.metainfo.xml
```

CI (`.github/workflows/ci.yml`) runs all of the above. It builds and tests
inside Arch, Fedora, Debian 13 and Ubuntu 24.04 containers, using each distro's
own package names from the matrix above, and checks the MSRV separately.

## Security & Auditing

This section is the audit map. Each item names the mechanism and where it
lives.

### Threat model

| Asset | Threat | Mitigation |
|-------|--------|------------|
| Filester API key | Disclosure via disk, logs, process list, crash dumps, VCS | Secret Service storage, `SecretString`, log redaction, no argv/env use, `.gitignore` guards |
| API key in transit | Interception or downgrade | HTTPS-only base URL, rustls with platform verifier, no cleartext fallback |
| User files | Exfiltration to the wrong host | Single compile-time API origin, cross-origin redirects refused |
| Local system | Memory corruption, resource exhaustion | `forbid(unsafe_code)`, streamed uploads, bounded channels |
| Build pipeline | Dependency or CI compromise | `Cargo.lock`, `cargo-deny`, SHA-pinned actions, read-only CI token |

### Token handling

- **At rest** (`src/secrets.rs`): The API key is stored only in the desktop
  Secret Service (GNOME Keyring, KWallet, KeePassXC) through
  [`oo7`](https://crates.io/crates/oo7), under the attributes
  `application=io.github.scroguard.Filester` and `kind=api-key`. Inside a
  Flatpak sandbox, oo7 automatically uses the XDG Secret portal instead. The
  key is never written to a config file, cache, or GSettings.
- **In memory** (`src/api/auth.rs`): The key is validated as 8–512 visible
  ASCII characters, which rules out CR/LF header injection. It is held in
  `secrecy::SecretString`, which zeroizes on drop and is redacted from `Debug`.
  Plaintext is exposed by exactly two `pub(crate)` functions,
  `authorization_header` and `expose_for_storage`; `grep expose_` lists every
  call site. The temporary `"Bearer …"` string is `Zeroizing`, and the header
  value is marked sensitive. *Limitation:* copies made inside hyper and rustls
  while the request is written are outside the app's control and are not
  zeroized.
- **Never via the process environment or argv**: There is no `--api-key` flag
  and no `FILESTER_API_KEY` variable, because both leak through
  `/proc/<pid>/cmdline`, `/proc/<pid>/environ` and shell history. The key is
  entered only in a `GtkPasswordEntry`, which is cleared as soon as the key
  is read. *Limitation:* GTK's internal text buffers are not zeroized.
- **Logging policy**: No log statement formats request headers, the token,
  URLs, or response bodies. Transport errors have their URL stripped
  (`reqwest::Error::without_url`), so search terms in query strings never
  reach logs. Server-supplied text is stripped of control characters and
  capped at 300 characters before it is stored in an error, which prevents
  log-line forgery. Unit tests assert that the `Debug` output of `ApiKey` and
  `FilesterClient`, and validation error messages, contain no key material.
- **Sign-out** (`ui/window.rs`): Cancels active uploads, drops the client
  (and with it the in-memory key), and deletes the keyring item. A 401 from
  any request triggers the same path, so a revoked key never lingers.

### Transport security

- The API origin is the compile-time constant `https://u1.filester.me`
  (`src/config.rs`), with no runtime override. `ClientConfig` rejects any base
  URL that is not `https`, or that carries credentials, a path, a query or a
  fragment. The single exception is plain `http` to a *literal* loopback IP
  (not the name `localhost`), which exists so the integration tests can use
  a local mock server. `https_only` is set on the HTTP client whenever the
  base is `https`. Path segments built from server-supplied IDs are
  percent-encoded, so an ID cannot inject `/`, `..`, `?` or `#`.
- TLS uses **rustls** (memory-safe), at TLS 1.2 or newer, with certificate
  verification delegated to `rustls-platform-verifier`, which reads the
  distro's trust store. This
  respects enterprise CAs installed via `update-ca-trust` or
  `update-ca-certificates`. There is no option to disable verification.
- The redirect policy allows **same-origin redirects only**, at most 5 hops.
  A redirect to another host is a hard error (`ApiError::CrossOriginRedirect`)
  rather than being followed, so the `Authorization` header can never be
  forwarded off-origin. An integration test covers this.
- Every request has explicit timeouts: connect 15 s, per-read 60 s, and a
  60 s total for ordinary calls. Uploads have no total timeout. Instead, a
  watchdog fails the transfer if no body bytes are accepted for 120 s, then
  allows 10 minutes for the server's reply once the body is sent.
- JSON response bodies are capped at 32 MiB, and error bodies at 64 KiB.
  Larger responses are refused rather than buffered.

### Rate limiting and abuse resistance (`src/api/rate_limit.rs`)

- `X-RateLimit-Limit`, `X-RateLimit-Remaining` and `X-RateLimit-Reset` are
  parsed on every response. Requests are delayed proactively when the
  remaining budget hits zero. The API docs do not say whether `Reset` is an
  epoch timestamp or a delta, so values ≥ 10⁹ are read as Unix seconds and
  smaller ones as seconds-from-now. The state is shared by every clone of the
  client. As of October 2026 the live API sends none of these headers, so in
  practice the budget is enforced by the 429 handling below.
- HTTP 429 triggers exponential backoff with full jitter. A `Retry-After`
  header (delta-seconds or HTTP-date) always takes precedence and acts as a
  lower bound. Retries are capped at 5 attempts. Any single wait longer than
  120 s is not taken silently: it surfaces as `ApiError::RateLimited` with
  the required wait, so the UI can inform the user.
- Non-idempotent requests (uploads) are retried only when the server
  explicitly answered 429, meaning the request was rejected before
  processing.

### Data isolation

- File contents are never cached locally by the app. Uploads are read
  straight from the user-selected file and streamed in 256 KiB chunks, so
  peak memory is independent of file size. The size is re-checked on every
  attempt and the stream is capped at that length, so a file modified
  mid-upload fails the transfer instead of sending inconsistent data. Only
  the file's base name is sent, never its directory.
- Folder and file metadata lives in process memory only and is discarded on
  exit.
- The app writes no files at all, not even preferences. The only persistent
  state is the keyring item.
- Server-supplied names and messages are untrusted. They go only into
  plain-text widget properties (`Label::set_text`, tooltips, `AdwAlertDialog`
  heading and body). The two libadwaita properties that parse Pango markup by
  default are neutralised in `ui/format.rs`: toasts are created with
  `use-markup = false`, and `AdwStatusPage` descriptions are passed through
  `glib::markup_escape_text`. The app never writes server-named files to
  disk; there is no download feature.
- Share links are opened or copied only if they are `https` URLs on
  `filester.me` or one of its subdomains, with no embedded credentials
  (`ui/format.rs::is_safe_share_url`, unit-tested against look-alike hosts).
- New folders are **private by default**. The API's own default is public,
  so the dialog offers public visibility as an explicit opt-in.

### Concurrency model

- The GTK main thread never performs I/O. A dedicated 4-thread tokio runtime
  (`src/worker.rs`) runs all networking, file reading and tree
  reconstruction. Results return through executor-agnostic oneshot channels
  that the UI awaits with `glib::spawn_future_local` (`ui::spawn_ui`). Each
  folder load carries a generation number, so a slow response for a folder
  the user has left is discarded rather than shown.
- Progress events use a bounded, lossy channel: a slow UI drops intermediate
  ticks rather than blocking the upload or growing memory.
- Dropping the worker cancels outstanding tasks without blocking the UI at
  exit. Aborting an upload task drops the HTTP request.

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
