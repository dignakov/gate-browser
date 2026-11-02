Extensions built into Gate, loaded unpacked at startup.

- `ubol/`: uBlock Origin Lite 2026.1026.1812 (Chromium build), from
  https://github.com/uBlockOrigin/uBOL-home/releases — the release zip,
  unpacked unchanged. GPL-3.0 (see `ubol/LICENSE.txt`). To update, replace
  the folder's contents with a newer release zip.

The browser never runs this copy: it installs it into the profile
(`extensions/ubol` there) on first launch, or when this one is newer, and
updates that copy from GitHub releases (see `crates/app/src/ublock.rs`).
