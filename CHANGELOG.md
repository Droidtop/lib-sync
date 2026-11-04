# Changelog

All notable changes to lib-sync. Versions follow [Semantic Versioning](https://semver.org/)
(0.x releases may change anything).

## [0.1.0]

### Added
- `sync-core`: identity, pairing, the session channel, the protocol, the library change log and the save-sync rules. Moved from droidtop-agent `0ec4110` `crates/core` unchanged but for its package name (Droidtop/tracker#867 step 1).
- `sync-store`: this computer's state on disk: identity shared with windowcast, paired devices, settings, the library. Moved from droidtop-agent `0ec4110` `crates/store` unchanged but for its package name (Droidtop/tracker#867 step 1).
- `sync-pair`: pairing a device: typing the code the other shows, or showing one. Moved from droidtop-agent `0ec4110` `crates/pair` unchanged but for its package name (Droidtop/tracker#867 step 1).
