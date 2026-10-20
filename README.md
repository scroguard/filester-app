# Filester Manager

A native GTK4 / libadwaita desktop client for the [Filester](https://filester.me)
cloud storage service, written in Rust.

> **Unofficial.** This project is not affiliated with, endorsed by, or supported
> by Filester. It talks to the public Filester HTTP API using a key you supply.
> It is installed as **Filester Manager** so it is not mistaken for an
> official Filester app. The package and command are `filester-app`.

[![CI](https://github.com/scroguard/filester-app/actions/workflows/ci.yml/badge.svg)](https://github.com/scroguard/filester-app/actions/workflows/ci.yml)
License: GPL-3.0-or-later · Picking the project up? Start with [HANDOFF.md](HANDOFF.md).

---

## Project status

| Phase | Scope | State |
|-------|-------|-------|
| 1 | Repository layout, build configuration, CI, desktop metadata, application shell | **Done** |
| 2 | API engine: async HTTP, token storage, rate limiting/backoff, streamed multipart uploads, folder-tree reconstruction, unit tests | **Done** |
| 3 | Main window: sidebar navigation, file/folder grid, transfer progress, event handling | **Done** |
| — | Open public files in local apps, Preferences, download-cache cleanup | **Done** |

## Features

- Sign in with an API key. The key is validated against the API, then
  stored in the system keyring, so later launches sign in automatically.
- Sidebar folder tree, rebuilt from parent links. It repairs orphaned or
  cyclic folders instead of hiding them.
- Icon grid of subfolders and files. Double-click a folder to open it, or a
  file to open it. Breadcrumbs appear in the header.
- Opening a file shows its share page in the browser by default. In
  Preferences you can choose **Open in Default App** instead: public files
  are then downloaded to a local cache and opened with the app registered
  for their type. Private files always open in the browser (see
  [Downloads and the local cache](#downloads-and-the-local-cache)).
- Optional automatic cleanup of the download cache: at every start, files
  not opened for a chosen number of days (default 7) are removed. The cache
  size and a Clear button are in Preferences, and signing out empties it.
- Uploads from the Upload button or by drag-and-drop, and downloads for
  opening locally. Two of each run at a time, the rest queue, and each has a
  progress bar and a cancel button. Closing the window with transfers in
  progress asks first.
- Multiple selection: Ctrl+click, Shift+click, drag a box on empty space,
  or Ctrl+A. Delete, Move and Copy Link act on everything selected.
- Move files and folders with **Move to…** (right-click or Ctrl+M), which
  lists every folder and greys out the invalid ones, or by dragging the
  selection onto a folder tile or a folder in the sidebar. The dialog warns
  when the destination is public.
- New folder (private by default), delete with confirmation, copy share links.
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
| <kbd>Ctrl</kbd>+<kbd>A</kbd> / <kbd>Escape</kbd> | Select all / clear selection (file grid focused) |
| <kbd>Delete</kbd> | Delete selected items (file grid focused) |
| <kbd>Ctrl</kbd>+<kbd>M</kbd> | Move selected items (file grid focused) |
| <kbd>Ctrl</kbd>+<kbd>C</kbd> | Copy share links, one per line (file grid focused) |
| <kbd>Ctrl</kbd>+<kbd>,</kbd> | Preferences |
| <kbd>Ctrl</kbd>+<kbd>W</kbd> / <kbd>Ctrl</kbd>+<kbd>Q</kbd> | Close / quit |

Delete, Ctrl+M and Ctrl+C are bound only to the file grid, so they can
never fire while you're typing in a text field.

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
│   └── workflows/
│       ├── ci.yml                # fmt, clippy, 4-distro build/test, MSRV, metadata, shellcheck, cargo-deny
│       └── release.yml           # tag → .deb, .rpm, Arch package, tarball, checksums, GitHub Release
├── data/
│   ├── icons/hicolor/scalable/apps/io.github.scroguard.Filester.svg
│   ├── io.github.scroguard.Filester.desktop
│   └── io.github.scroguard.Filester.metainfo.xml
├── src/
│   ├── main.rs                   # binary: logging + GApplication (all GTK code)
│   ├── lib.rs                    # library root: everything testable headlessly
│   ├── config.rs                 # app ID, API origin, download route + host allowlist, User-Agent
│   ├── api/                      # REST engine — no GTK imports allowed here
│   │   ├── mod.rs                #   public facade and re-exports
│   │   ├── auth.rs               #   ApiKey: validation, redaction, sensitive header
│   │   ├── client.rs             #   FilesterClient: endpoints, retries, redirects, size caps
│   │   ├── rate_limit.rs         #   X-RateLimit-* tracking, 429 + Retry-After backoff
│   │   ├── upload.rs             #   streamed multipart body, progress, stall watchdog
│   │   ├── download.rs           #   public-file downloads (keyless, host allowlist, size checks)
│   │   ├── models.rs             #   serde wire types (lenient decoding)
│   │   ├── tree.rs               #   folder hierarchy from parent-ID links, anomaly repair
│   │   └── error.rs              #   ApiError, server-text sanitising
│   ├── cache.rs                  # download cache: safe names, 0700/0600, age-based cleanup
│   ├── secrets.rs                # Secret Service / portal token store (oo7)
│   ├── worker.rs                 # tokio runtime ↔ GTK bridge (oneshot + async-channel)
│   └── ui/                       # GTK front end (binary only)
│       ├── mod.rs                #   Location, spawn_ui (worker → main-thread bridge)
│       ├── window.rs             #   window, sign-in, navigation, actions, dialogs
│       ├── sidebar.rs            #   TreeListModel folder tree + storage gauge
│       ├── file_grid.rs          #   GridView tiles + context menu
│       ├── transfers.rs          #   upload/download queue with progress bars
│       ├── preferences.rs        #   Preferences dialog (open mode, cache retention)
│       ├── settings.rs           #   settings.ini load/save, XDG paths
│       └── format.rs             #   error text, share-URL validation, markup safety
├── packaging/arch/PKGBUILD       # Arch / AUR package (builds the tagged source)
├── install.sh                    # per-user or system install/uninstall, menu entry + icon
├── tests/api_client.rs           # end-to-end client tests against a wiremock server
├── tests/download.rs             # download + cache tests (incl. "no Authorization header")
├── tests/keyring_live.rs         # manual, read-only check against the real keyring
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

## Installing

### From a release (no Rust needed)

Each tagged release on the
[Releases page](https://github.com/scroguard/filester-app/releases) has
native packages and a universal tarball, all for x86_64:

| Distribution | Download | Install |
|--------------|----------|---------|
| Debian 13, Ubuntu 24.04+, Linux Mint 22+ | `filester-app_<version>-1_amd64.deb` | `sudo apt install ./filester-app_*.deb` |
| Fedora (current releases) | `filester-app-<version>-1.x86_64.rpm` | `sudo dnf install ./filester-app-*.rpm` |
| Arch Linux, CachyOS | `filester-app-<version>-1-x86_64.pkg.tar.zst` | `sudo pacman -U ./filester-app-*.pkg.tar.zst` |
| Any of the above, RHEL 10 and clones, older Fedora | `filester-app-<version>-x86_64-linux.tar.gz` | unpack, then `./install.sh` |

The `.rpm` and Arch packages are built on the newest Fedora and Arch, so
they need an equally current system. The tarball is built on Ubuntu 24.04,
the oldest C library (glibc 2.39) in the support matrix, and runs on every
supported distribution.

The packages pull in GTK 4 and libadwaita automatically and put Filester Manager in
your application menu. Remove them with your package manager as usual
(`sudo apt remove filester-app`, `sudo dnf remove filester-app`,
`sudo pacman -R filester-app`).

Verify a download before installing:

```sh
sha256sum -c SHA256SUMS --ignore-missing
gh attestation verify filester-app_*.deb --repo scroguard/filester-app   # built by this repo's CI
```

The tarball contains a prebuilt binary and the installer described next. It
needs GTK 4.14+ and libadwaita 1.5+ installed (see
[Runtime dependencies](#runtime-dependencies)). The installer checks for them
and prints the right package command if any are missing.

### With the installer script

`install.sh` installs Filester Manager and its menu entry and icon. It works from an
unpacked release tarball (using the bundled binary) or from a source
checkout (building with cargo first, which needs the
[build dependencies](#build-dependencies)):

```sh
./install.sh                # just you: ~/.local/bin, ~/.local/share (no root)
./install.sh --system       # everyone: /usr/local (asks for sudo for the copy step only)
./install.sh --prefix DIR   # anywhere: DIR/bin, DIR/share
```

The menu entry points at the installed binary by absolute path, so it works
even if `~/.local/bin` is not on your `PATH`. The build never runs as root.

To remove it, repeat the same options with `--uninstall`. Add `--purge` to
also delete your settings and download cache (per-user only). The API key in
your keyring is kept; the uninstaller prints the command that removes it.

```sh
./install.sh --uninstall [--system | --prefix DIR] [--purge]
```

### Arch User Repository

`packaging/arch/PKGBUILD` builds a tagged release from source and is ready
to publish to the AUR. To build it yourself:

```sh
cd packaging/arch && makepkg -si
```

### Making a release (maintainers)

1. Bump the version in `Cargo.toml` (then `cargo check` to update
   `Cargo.lock`), `pkgver` in `packaging/arch/PKGBUILD`, and add a
   `<release>` entry to `data/io.github.scroguard.Filester.metainfo.xml`.
2. Commit, then tag and push: `git tag -s v0.2.0 && git push origin v0.2.0`.
3. `.github/workflows/release.yml` checks that the versions match, builds
   the `.deb` and tarball (Ubuntu 24.04), `.rpm` (Fedora) and Arch package,
   installs each one in a clean container, then publishes a GitHub Release
   with `SHA256SUMS` and signed build-provenance attestations.

Run the workflow manually (Actions → Release → Run workflow) to build and
test the packages without publishing anything.

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

### Outside the documented API: public downloads

The API has no download endpoint. For **Open in Default App**,
`PublicDownloader` (`src/api/download.rs`) uses the route the website's
share page uses:

| Request | Purpose |
|---------|---------|
| `POST https://filester.me/v2/api/public/download` `{"file_slug": …}` | short-lived token, file key, and the server holding the bytes |
| `GET https://<server>/v2/<file>?token=…&download=true` | the file contents |

This route is undocumented and may change without notice. It works for
public files only. A file is private if it, or any folder above it, is
private. The route refuses private files with HTTP 403 even when the API key
is supplied (tested October 2026), so private files cannot be opened locally.
Observed servers are `*.cdn.cr` (with `*.filester.me` mirrors as the site's
fallback).

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
| Downloaded files | Disclosure to other local users, path traversal, unbounded disk use | Owner-only cache (0700/0600), sanitised names, exact size checks, retention cleanup, emptied on sign-out |
| API key | Leak to download hosts | Download client never holds the key; test asserts no `Authorization` header |
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
- **Sign-out** (`ui/window.rs`): Cancels active transfers, empties the
  download cache, drops the client (and with it the in-memory key), and
  deletes the keyring item. A 401 from
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

### Downloads and the local cache

Off by default: nothing is downloaded unless the user picks **Open in
Default App** in Preferences, whose text states that files are stored on
this computer.

- **Separate client, no key** (`src/api/download.rs`): `PublicDownloader`
  has its own HTTP client and has no access to the API key, so the key
  stays confined to `u1.filester.me`. `tests/download.rs` asserts that no
  download request carries `Authorization` or `Cookie`.
- **Host allowlist**: the server named in a token response is untrusted. It
  must be `https`, have no credentials, path, query or fragment, and its
  host must be in `config::DOWNLOAD_HOSTS` (`filester.me`, `cdn.cr`, or a
  subdomain of either). Redirects are held to the same rule. Anything else
  fails with `ApiError::UntrustedDownloadServer` before it is contacted, so
  a change of CDN breaks downloads loudly rather than silently trusting a
  new host.
- **Integrity and size**: the body is streamed to disk and must match the
  size the API reported, exactly. `Content-Length` is checked before
  writing, and the stream is cut off if it runs long. A short body fails.
  Downloads are capped at 10 GiB. Failed downloads delete their partial
  file. The token sits in the request URL, and URLs are stripped from
  stored errors and never logged.
- **Cache layout** (`src/cache.rs`):
  `$XDG_CACHE_HOME/io.github.scroguard.Filester/files/<slug>/<name>`.
  Directories are created `0700` (tightened if they already exist) and
  files `0600`, with `O_EXCL`, so nothing pre-existing is followed or
  overwritten. A download is written to a hidden `.part` file and renamed
  into place, so a completed name only ever holds a complete file. The
  slug must be 1–64 characters of `[A-Za-z0-9_-]`, and the name goes
  through `safe_file_name`: separators and control characters become `_`,
  leading dots are removed, and it is shortened to 200 bytes keeping the
  extension. Downloaded files are never marked executable.
- **Cleanup**: at every start, partial downloads older than a day are
  removed, and, if **Remove Old Files Automatically** is on, so are files
  not opened within the chosen number of days (1–365, default 7). Opening a
  cached file refreshes its timestamp. Cleanup uses `symlink_metadata`
  throughout, never follows symlinks, and never descends below the slug
  directories. Preferences shows the cache size and offers Clear, and
  sign-out empties the cache completely.
- **Opening** goes through `GtkFileLauncher`, which defers to the desktop's
  default handler or, in a sandbox, the `OpenURI` portal.

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

- File contents are cached locally only when the user turns on **Open in
  Default App** (see above). Uploads are read straight from the
  user-selected file and streamed in 256 KiB chunks, so
  peak memory is independent of file size. The size is re-checked on every
  attempt and the stream is capped at that length, so a file modified
  mid-upload fails the transfer instead of sending inconsistent data. Only
  the file's base name is sent, never its directory.
- Folder and file metadata lives in process memory only and is discarded on
  exit.
- Besides the keyring item and the optional download cache, the app writes
  one file: `$XDG_CONFIG_HOME/io.github.scroguard.Filester/settings.ini`
  (mode `0600`, written atomically). It holds only the open mode and cache
  retention, never secrets, and invalid values fall back to defaults.
- Server-supplied names and messages are untrusted. They go only into
  plain-text widget properties (`Label::set_text`, tooltips, `AdwAlertDialog`
  heading and body). The two libadwaita properties that parse Pango markup by
  default are neutralised in `ui/format.rs`: toasts are created with
  `use-markup = false`, and `AdwStatusPage` descriptions are passed through
  `glib::markup_escape_text`. Server-supplied names reach the filesystem
  only as cache file names, after `safe_file_name`.
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
  exit. Aborting a transfer drops the HTTP request. An aborted download
  leaves a `.part` file, which the next cleanup removes.
- Cache scans and deletions run on the worker via `spawn_blocking`, never
  on the GTK thread. Loading `settings.ini` at startup is the one small
  synchronous read, as with `GSettings`.

### Code and supply chain

- `unsafe_code = "forbid"` crate-wide. Clippy runs at `pedantic` level in CI
  with `-D warnings`, and `unwrap_used`, `panic`, `todo`, `unimplemented` and
  `dbg_macro` are denied in non-test code.
- Release builds keep `overflow-checks = true`.
- `Cargo.lock` is committed, and every CI cargo command uses `--locked`.
- `cargo-deny` (`deny.toml`) enforces RustSec advisories (including yanked and
  unmaintained crates), a GPL-compatible licence allow-list, crates.io as the
  only source, no wildcard versions, and a ban on `openssl-sys` and
  `native-tls`. Every ignored advisory is listed by ID in `deny.toml` with
  its reason. Currently the only ones are RUSTSEC-2026-0370/0371/0372,
  "unmaintained" notices (no vulnerability) for `zbus` 5 sub-crates that
  `oo7` 0.6 depends on; they go once a stable `oo7` on `zbus` 6 ships.
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
