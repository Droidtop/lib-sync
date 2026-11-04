# lib-sync

Device identity, pairing, the session channel, discovery and transports, the protocol and the sync rules, and this computer's agent state on disk. One of the shared library families of Droidtop/tracker#867: the libraries belong to no program;
each program has its own scaffold and pins a tagged release of each library it uses (#867 D2).

| Crate | Where | Holds |
|---|---|---|
| `sync-core` | [`crates/core`](crates/core) | identity, pairing, the session channel, the protocol, the library change log and the save-sync rules |
| `sync-store` | [`crates/store`](crates/store) | this computer's state on disk: identity shared with windowcast, paired devices, settings, the library |
| `sync-pair` | [`crates/pair`](crates/pair) | pairing a device: typing the code the other shows, or showing one |

Used by: droidtop-agent (and through it droidtop's `libdroidtop_agent.so`); Pythia once its native module moves on.

Licence: GPL-3.0-only (see `LICENSE`).

## Where the code came from

#867 step 1 moves existing code before changing it ("reuse and contract first, rewrites second"). These
crates are droidtop-agent's, moved as they were:
  - `sync-core` from droidtop-agent `0ec4110` `crates/core`
  - `sync-store` from droidtop-agent `0ec4110` `crates/store`
  - `sync-pair` from droidtop-agent `0ec4110` `crates/pair`

Only the package names changed. Code still imports each crate by its old name (`droidtop_agent_*`), so
nothing that uses it had to be edited; renaming the imports is a later, mechanical change.

## Build and test

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI (`.github/workflows/ci.yml`) runs those on Linux, Windows and macOS. It then builds and tests every
program that uses this library against the commit (#867 D2). Work lands on `main` from `ci/*` branches
once CI is green.

## Versions

A release is a tag `vX.Y.Z`. CI creates it on a green `main` whose root `Cargo.toml` names a version not
yet tagged, so to release, raise that version (and the CHANGELOG) in the change that needs it. Programs,
and the other libraries, depend on a tag:

```toml
droidtop-agent-core = { package = "sync-core", git = "https://github.com/Droidtop/lib-sync", tag = "v0.1.0" }
```
