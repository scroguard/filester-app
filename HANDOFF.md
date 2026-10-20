# Handoff

Use this to get back into the project quickly. The [README](README.md) has the
full design and security write-up. This file covers where things stand, where
to make changes, and the traps found the hard way.

_Last updated 2026-10-19, at release v0.1.2._

## Current state

- **Released:** [v0.1.2](https://github.com/scroguard/filester-app/releases/tag/v0.1.2)
  ships `.deb` (Debian 13 / Ubuntu 24.04+), `.rpm` (Fedora), an Arch
  `.pkg.tar.zst` and a universal tarball with `install.sh`, plus
  `SHA256SUMS` and build-provenance attestations.
- **Working:**
  - API-key sign-in stored in the Secret Service.
  - Folder sidebar and file grid.
  - Streamed uploads up to 10 GB (button or drag-and-drop).
  - Multiple selection in the grid (Ctrl/Shift+click, box select, Ctrl+A).
  - Move via "Move to…" (Ctrl+M) or drag-and-drop onto a folder tile or
    sidebar folder; bulk delete; copy links; new folder; storage gauge.
    Moves are sent in requests of at most `MAX_BULK_ITEMS` (200). Deletes
    are limited by the API to 100 identifiers per request, so
    `delete_files` / `delete_folders` split longer lists themselves
    (`MAX_DELETE_ITEMS`). No rename call is implemented.
  - Rate-limit handling with `Retry-After`.
  - Opening files in the browser (default), or opening public files in local
    apps through a cache that is cleaned automatically.
  - Preferences dialog.
- **CI** (`ci.yml`) is green on Arch, Fedora, Debian 13, Ubuntu 24.04 and the
  MSRV (1.92). It also runs clippy (pedantic), rustfmt, cargo-deny,
  the desktop and AppStream validators, and shellcheck.
- **Naming:** the display name is **Filester Manager**, so the app isn't
  mistaken for an official Filester app. The package, the command
  (`filester-app`) and the app ID (`io.github.scroguard.Filester`) are
  deliberately unchanged. Renaming any of them breaks upgrades and orphans
  users' settings, cache and keyring entries.

## Getting started

```sh
cargo run                                           # debug build
FILESTER_LOG=filester_app=debug cargo run           # verbose logs (stderr)
cargo fmt --all && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked
```

System dependencies for each distro are listed in
[README → Dependency matrix](README.md#dependency-matrix). The toolchain
comes from `rust-toolchain.toml` (stable). The lints in `Cargo.toml` deny
`unwrap`, `expect`, `panic`, `print*` and similar outside tests, and treat
pedantic clippy warnings as errors in CI.

## Code map

| Area | File(s) | Notes |
|------|---------|-------|
| Constants (app ID, display name, API host, download hosts) | `src/config.rs` | Hosts are compile-time only. The API key can't be redirected by config. |
| HTTP client, retries, redirects, size caps | `src/api/client.rs` | Every endpoint lives here (`FilesterClient`). |
| Rate limit (1000 req/h) and 429 backoff | `src/api/rate_limit.rs` | |
| Streamed multipart upload | `src/api/upload.rs` | |
| Public-file download (undocumented route) | `src/api/download.rs` | Uses a separate client with no key and no cookies. Allowlists hosts and enforces the exact file size. |
| Folder tree from parent IDs | `src/api/tree.rs` | |
| Errors and API models | `src/api/error.rs`, `src/api/models.rs` | User-facing wording for each error is in `src/ui/format.rs`. |
| Keyring | `src/secrets.rs` | oo7. Attributes: `application=<app id>`, `kind=api-key`. |
| Download cache | `src/cache.rs` | Directories are 0700 and files 0600, created with `O_EXCL`. Cleanup is symlink-safe. |
| Background runtime | `src/worker.rs` | **The GTK thread never blocks.** All I/O goes through `Worker::spawn` / `ui::spawn_ui`. |
| Main window, actions, menus | `src/ui/window.rs` | The largest file. Start here for UI changes. |
| Sidebar, grid, activity panel | `src/ui/sidebar.rs`, `file_grid.rs`, `transfers.rs` | The grid uses `MultiSelection`. Drags carry `DraggedItems` as an in-process GObject value. The activity panel (`transfers.rs`) shows uploads, downloads, deletions and moves; deletions and moves share a one-at-a-time queue and report progress per batch. |
| Move dialog and move rules | `src/ui/move_dialog.rs` | `check()` is shared by the dialog and drag-and-drop, and is unit tested. |
| Preferences and settings file | `src/ui/preferences.rs`, `src/ui/settings.rs` | `$XDG_CONFIG_HOME/io.github.scroguard.Filester/settings.ini` |
| Desktop entry, AppStream, icon | `data/` | Validated in CI. |
| Packaging | `Cargo.toml` (`[package.metadata.deb]`, `[package.metadata.generate-rpm]`), `packaging/arch/PKGBUILD`, `install.sh` | |
| Release pipeline | `.github/workflows/release.yml` | |
| Tests | `tests/api_client.rs`, `tests/download.rs` (wiremock), `tests/keyring_live.rs` (manual, `--ignored`, read-only) | Unit tests sit inside the modules. |

Conventions:

- UI components are plain structs that own their widgets, with no GObject
  subclassing.
- Signal handlers hold `Weak` references.
- Untrusted text goes only into plain-text widget properties, never Pango
  markup. See the comment at the top of `src/ui/mod.rs`.

## Making a release

1. Bump the version in three places, plus the lockfile. The `versions` job
   fails the release if they disagree.
   - `version` in `Cargo.toml`, then run `cargo check` (or
     `cargo update -p filester-app`) to update `Cargo.lock`.
   - `pkgver` in `packaging/arch/PKGBUILD`. Reset `pkgrel=1`.
   - A new `<release version=… date=…>` at the **top** of `<releases>` in
     `data/io.github.scroguard.Filester.metainfo.xml`.
2. Run `appstreamcli validate --no-net data/*.metainfo.xml`, then commit and
   push to `main`.
3. To try the packaging without publishing, use Actions → Release → Run
   workflow.
4. Tag and push: `git tag -a vX.Y.Z -m "Filester Manager X.Y.Z" && git push origin vX.Y.Z`.
   (The README suggests `git tag -s`; no signing key is set up yet, so
   v0.1.0 to v0.1.2 used `-a`.)
5. The workflow builds all four packages, installs each in a clean
   container, then publishes the release. Expect it to take about 15 minutes.
6. Verify:
   `gh release download vX.Y.Z && sha256sum -c SHA256SUMS`, then
   `gh attestation verify <file> --repo scroguard/filester-app --format json`.
   Without `--format json` it prints nothing when run outside a TTY.

## Known traps

- **Fedora mirror flakes.** The `.rpm` job for v0.1.1 failed with a curl
  timeout downloading `openh264` from `codecs.fedoraproject.org`. That's an
  optional codec pulled in by GTK's dependencies, not our code.
  `gh run rerun <id> --failed` fixed it, and `publish` then ran normally.
  Possible hardening: `dnf --setopt=install_weak_deps=False` and/or
  `--disablerepo=fedora-cisco-openh264` in `release.yml` and `ci.yml`.
- **makepkg emits a `-debug` split package.** `release.yml` filters it out,
  and the job checks that exactly one package is left. v0.1.0 accidentally
  published it; that asset was deleted by hand.
- **`cargo generate-rpm --output dist/`** treats `dist/` as a file name
  unless the directory already exists. That's why the job runs `mkdir -p dist`.
- **Workflow scripts run under `bash -e`.** `! ldd … | grep 'not found'` does
  not fail the step. Use `if ldd … | grep 'not found'; then exit 1; fi`.
- **Arch dependency names.** `libgcc_s` now comes from `libgcc`, not
  `gcc-libs`.
- **Pinning.**
  - Actions are pinned to commit SHAs, and cargo-deb (3.8.0) and
    cargo-generate-rpm (0.21.0) to exact versions, in `release.yml`'s `env`.
  - When bumping any of these, pick a release at least two weeks old and
    update the SHA together with its `# vX.Y.Z` comment.
- **cargo-deny ignores.** `deny.toml` ignores the zbus 5 "unmaintained"
  advisories, which come in through oo7. Each ignore has a reason. Recheck
  them whenever oo7 updates.

## Filester API facts (verified live, October 2026)

- The REST API is at `https://u1.filester.me`. `filester.me/api/v1/...`
  returns 404. The documentation is at <https://filester.me/api-docs>.
- **Private files can't be downloaded through the API, even with the key.**
  So "open in local app" only works for public files, and private files
  always fall back to the browser.
- **Public downloads use an undocumented route.**
  - The route is `PUBLIC_DOWNLOAD_TOKEN_URL`: the website's own download
    endpoint, which hands out short-lived download links.
  - The files themselves are served from `*.cdn.cr` and `*.filester.me`.
  - **This is the most likely thing to break.** If downloads start failing,
    compare against `/js/file_dl.js` on the share page and update
    `src/config.rs` and `src/api/download.rs`.
  - `tests/download.rs` mocks the expected behaviour.
- Share slugs come from the `slug` field, or are parsed from
  `https://filester.me/d/<slug>`.

## Testing against the live service

- The owner's account has real data. **Never delete anything that already
  existed there.** Put all test uploads and folders in the
  `filester-app_test` folder, which already holds test files from
  development and may be cleaned up whenever you like.
- Ask the owner for an API key; none is stored anywhere in this repository.
  Paste it into the app's sign-in dialog, which saves it in the keyring.
  Never commit a key, put it in a command line or an environment variable,
  or print it in logs. `.gitignore` has guard patterns for this.
- `tests/keyring_live.rs` only reads from the keyring. Writing to the
  developer's real keyring needs the owner's explicit permission.

## Open ideas (not started)

- [ ] Publish `packaging/arch/PKGBUILD` to the AUR. The PKGBUILD is ready and
      builds from the tag.
- [ ] Flatpak manifest. The planned permissions are described in
      [README → Flatpak](README.md#flatpak-planned).
- [ ] Fedora job hardening against the openh264 mirror (see Known traps).
- [ ] Ask for confirmation before opening a very large public file locally,
      since it is downloaded to the cache first.
- [ ] Signed tags (`git tag -s`) once a signing key is set up.

## Housekeeping

- **Downloads in the repo root.** These files are not part of the repo:
  - `filester-app-0.1.0-1-x86_64.pkg.tar.zst` (ignored by git).
  - `SHA256SUMS` (**not** ignored, so don't commit it by accident).
- **Commit style.** Short imperative subject; a body explaining *why*.
  Separate commits per logical change.
