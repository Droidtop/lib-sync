# Changelog

All notable changes to lib-sync. Versions follow [Semantic Versioning](https://semver.org/)
(in 0.x, only a new minor version may break the interface; CI's `semver` job enforces it).

## [Unreleased]

## [0.2.0]

### Added
- `sync-core::locked`: what droidtop locks on the person's shares, in age's format (age-encryption.org/v1), so they
  can open it themselves with the age or rage tools (Droidtop/tracker#573; owner, 2026-11-05: "We should use
  well-known crypto and encapsulation mechanisms, so the user can retrieve that data themselves"). A `Keyring` of the
  person's share keys (X25519):
  - `generate`, and `merge` as a union, so devices that each made a key keep both;
  - `lock` to the newest key, and `open` with any key;
  - its text is an age identity file (`to_text`, `from_text`, which also reads age-keygen's files);
  - `export_with_passphrase` (age's scrypt mode) and `import`.

### Changed (breaking)
- `sharesaves::ShareSaveRequest`: `sealed: bool` and `key: &DeviceKey` are replaced by `lock: Option<&Keyring>`.
  A locked part of a share now holds age files that open with any key in the person's ring, instead of the agent's
  device-to-device sealing, which only the writing device could open. A program sets `lock` to its share keyring for
  a locked part, or None for a plain one. No program used the old fields yet.


## [0.1.7]

### Added
- `sync-core`: feature `net-remote`, off by default. `share::RemoteShare` makes any lib-net share (WebDAV, SFTP, S3)
  a `Share`, so store and forward and save sets go straight through it (Droidtop/tracker#573; owner, 2026-11-05: "A
  share can be a backend for a sync"). lib-net's `net-remote` is taken at tag `v0.2.1`. A program that doesn't turn
  the feature on never builds it, so the Rust 1.85 floor holds for everything else (net-remote itself needs 1.89).

## [0.1.6]

### Added
- `sync-core` tunnel: a side link (Droidtop/tracker#113). `TunnelStream::side()` hands the app, once, a `SideLink`:
  a second reliable byte stream through the same WireGuard session (inner TCP port 47614), beside the session
  channel, as two queues so one thread can read while another writes. A remote stream's signaling runs on it while
  its media rides `datagrams()`. An empty read is its end; dropping `outbound` closes it.

## [0.1.5]

### Changed
- Builds with Rust 1.85 again (`rust-version` 1.89 → 1.85). Pythia builds these crates with Ubuntu's rustc-1.85, as
  Launchpad does; 1.89 came from droidtop-agent b5e7094, only for `File::lock`. The store's lock is now fs4's
  `FileExt::lock` (the same operating system lock: flock on Unix, LockFileEx on Windows), and two
  `is_multiple_of` (1.87) are `%`. A new `msrv` CI job checks the workspace with 1.85, resolving dependencies to
  versions 1.85 can build, as a consumer's lock would.

## [0.1.4]

### Added
- `sync-core`: a game's saves kept on a share (`sharesaves::sync_with_share`, Droidtop/tracker#573). The same rule as
  a live sync (one side changed wins; both changed is `Outcome::Conflict`, settled by the person's choice; the side
  overwritten keeps a copy), with the share as the other side, so devices on one share keep each other's saves with
  no computer. On the share: `droidtop/saves/<digest of the game key>/` with `blobs/<digest>` (written once, never
  changed), `set.json` (written last, in one step) and `sets/` (a replaced set that had changed too). Locked
  (`ShareSaveRequest::sealed`): every blob and set sealed with the agent's sealing to this device's key, so the share
  holds ciphertext only; a set another device locked is refused and changes nothing. A share with no set never makes
  a device delete its saves: its own set is written there.
- `sync-core`: store and forward over any share (Droidtop/tracker#573, #800 item 3). `share::Share` names what sync
  needs from a share (list a folder's finished files, read, write in one step, remove one file, make folders), with
  `share::Folder` for a folder on this computer. A share client (lib-net's `net-remote`) is one more implementation
  of it (owner, 2026-11-05: "lib-sync controls synchronization. A share can be a backend for a sync"). New:
  `mailbox::post_via`, `collect_via`, `inbox_path`, `ShareLetter`, and `savesync::post_via`. `mailbox::post`,
  `collect` and `savesync::post_to_share` keep their signatures and go through `Folder`; a letter is now written to
  `.<name>.<tag>.part` before its rename.

## [0.1.3]

### Added
- `sync-core` tunnel: datagrams beside the stream (Droidtop/tracker#113). Each WireGuard session has an inner UDP
  socket next to its TCP link, and `TunnelStream::datagrams()` hands the app `Datagrams`, two queues of whole
  datagrams to and from the other end. So a remote stream's media rides the agent's own tunnel (owner, 2026-10-28:
  "Our standard carrier tunnel is also a remote streaming tunnel"). Unreliable and unordered, as UDP is; at most
  `Datagrams::MAX` bytes each.
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
