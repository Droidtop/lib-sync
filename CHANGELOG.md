# Changelog

All notable changes to lib-sync. Versions follow [Semantic Versioning](https://semver.org/)
(0.x releases may change anything).

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
