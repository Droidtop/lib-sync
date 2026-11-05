# Changelog

All notable changes to lib-sync. Versions follow [Semantic Versioning](https://semver.org/)
(in 0.x, only a new minor version may break the interface; CI's `semver` job enforces it).

## [Unreleased]

## [0.1.3]

### Added
- `sync-remote`, a new crate: one client for the person's own shares and cloud storage (Droidtop/tracker#573, design
  D1). The `Remote` interface (list, stat, read, ranged read, a write that lands in one step through a temporary name,
  rename, remove one file, make folders), checked share paths (`RemotePath`: no `..`, `.`, empty segment or
  backslash), and two backends: `LocalFolder` and `WebDav` (PROPFIND, GET with Range, PUT, MOVE, DELETE, MKCOL; Basic
  sign-in over https only unless the person allows plain http for that share; no redirects followed; a timeout on
  every request). CI tests WebDAV against a real server (wsgidav).
- `sync-core`: the store-and-forward mailbox over any share (Droidtop/tracker#800 item 3): `mailbox::post_to`,
  `collect_from` and `inbox_path` take a `Remote`. `post` and `collect` keep their signatures and behaviour and go
  through `LocalFolder`; a letter is now written to `.<name>.<tag>.part` before its rename, as every `Remote` write is.
  `savesync::post_to_remote` leaves a game's saves on any `Remote`; `post_to_share` goes through it.
- CI's `semver` job: `cargo-semver-checks` against the last release, so a breaking change fails unless its version and
  this CHANGELOG say so (Droidtop/tracker#867).

## [0.1.2]

### Added
- `sync-core`: cross-store identity, first slice (Droidtop/tracker#798, droidtop-agent docs/DESIGN.md section 16
  step 1). A third library change kind, `Change::Link {game, into, at}`: key `game` is the same game as key
  `into`, `None` undoes it, one per key with the newest winning, a self-link refused. `Library::link` makes one
  and `Library::canonical` follows links to the key a game is known by; a loop ends at its smallest key, so every
  device answers alike. Hello feature `identity` (`library::LINK_FEATURE`): the server keeps what the device's
  hello stated and leaves links out of `library_pull` for one that did not (`Change::readable_without_links`).
- `sync-store`: `Store::set_link`.

## [0.1.1]

### Changed
- `sync-core`: a device the computer no longer knows is told so. The accepting side sends one error with code
  `not_paired` through the finished channel before closing, reads and drops what the device sends until it
  hangs up, then sends close_notify, and `client::connect` stops at `ConnectError::NotPaired` instead of trying
  the other paths. From droidtop-agent 54a4a00 and 41f2d04 (Droidtop/tracker#531), which landed in
  droidtop-agent's copy of this crate after it moved here.

## [0.1.0]

### Added
- `sync-core`: identity, pairing, the session channel, the protocol, the library change log and the save-sync rules. Moved from droidtop-agent `0ec4110` `crates/core` unchanged but for its package name (Droidtop/tracker#867 step 1).
- `sync-store`: this computer's state on disk: identity shared with windowcast, paired devices, settings, the library. Moved from droidtop-agent `0ec4110` `crates/store` unchanged but for its package name (Droidtop/tracker#867 step 1).
- `sync-pair`: pairing a device: typing the code the other shows, or showing one. Moved from droidtop-agent `0ec4110` `crates/pair` unchanged but for its package name (Droidtop/tracker#867 step 1).
