//! A game's saves kept on one of the person's shares (Droidtop/tracker#573): the same rule as a live
//! sync with a computer (savesync: a side that alone changed wins; both changed is a conflict the
//! person settles; the side overwritten keeps a copy first), with the share as the other side. So two
//! devices on one share keep each other's saves, with no computer and no pairing.
//!
//! On the share, under `droidtop/saves/<game>/` (`<game>` is a digest of the game's key, so no title
//! or path is ever a file name there):
//! - `blobs/<digest>`: each file's stored bytes, named by the digest of those bytes. A blob is
//!   written once and never changed or deleted, so a set that names it can always be read.
//! - `set.json`: the current set, the files' entries and the blob each is in, the device that wrote
//!   it, and when. It is written last and in one step, so a reader sees the old set or the new one,
//!   never half of one; a sync cut short leaves the old set current.
//! - `sets/<UTC stamp>-<device>.json`: a set that was replaced while it had changed too (both sides
//!   changed and the person chose this device's). Nothing on the share is ever deleted.
//!
//! **Locked or plain, per part of a share** (owner, 2026-11-05: "When parts of a share (since it doesn't
//! have to be all or nothing) are encrypted, we can sync those keys, and let the user export the key"): with
//! [`ShareSaveRequest::lock`] every blob and every set is an age file locked to the person's share key
//! ([`crate::locked`]). The share holds ciphertext only. Any device with the key opens it, and so does the age tool
//! with the exported key. A set locked with a key this device doesn't have is refused, and nothing here changes.
//!
//! A share that has no set for a game (a new share, or one emptied) never makes this device delete its
//! saves: this device's set is written there instead.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::locked::{self, Keyring};
use crate::manifest::{self, FileEntry, Manifest};
use crate::proto::GameRef;
use crate::saves::{self, Roots, SaveSpec};
use crate::savesync::{copy_from, decide, keep_copy, save_set, utc_stamp, Outcome, Plan, Side};
use crate::share::Share;
use crate::{error::protocol, Result};

/// One sync of a game's saves with a share.
pub struct ShareSaveRequest<'a> {
    pub game: GameRef,
    pub spec: &'a SaveSpec,
    pub roots: &'a Roots,
    /// The baseline file for this share and game.
    pub baseline_path: &'a Path,
    /// This game's archive here: the copies kept per device, and older ones.
    pub archive_dir: &'a Path,
    /// This device's name, for the sets it writes and the copy it keeps of its own saves.
    pub name: &'a str,
    /// The person picked a side: the answer to [`Outcome::Conflict`].
    pub choice: Option<Side>,
    /// The person's share keys when this part of the share is locked: what is written is locked to the ring's
    /// newest key. None writes plain files. What is read opens with any key in the ring, whichever way it was written.
    pub lock: Option<&'a Keyring>,
}

/// The set on a share.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct StoredSet {
    pub device: String,
    pub written_ms: i64,
    pub files: Vec<StoredFile>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct StoredFile {
    #[serde(flatten)]
    pub entry: FileEntry,
    /// The blob holding its bytes.
    pub blob: String,
}

fn hex_digest(bytes: &[u8]) -> String {
    crate::hex::encode(&Sha256::digest(bytes))
}

/// The game's folder on a share.
pub fn game_dir(game: &GameRef) -> String {
    format!("droidtop/saves/{}", &hex_digest(game.key.as_bytes())[..32])
}

fn seal_if(req: &ShareSaveRequest, bytes: Vec<u8>) -> Result<Vec<u8>> {
    match req.lock {
        Some(ring) => ring.lock(&bytes),
        None => Ok(bytes),
    }
}

fn open_if_sealed(req: &ShareSaveRequest, bytes: Vec<u8>) -> Result<Vec<u8>> {
    if !locked::is_locked(&bytes) {
        return Ok(bytes);
    }
    match req.lock {
        Some(ring) => ring.open(&bytes),
        None => Err(protocol("this part of the share is locked; turn on its lock with your share key to read it")),
    }
}

/// The set on the share, or None when there is none yet.
pub fn read_set(share: &dyn Share, req: &ShareSaveRequest) -> Result<Option<StoredSet>> {
    let dir = game_dir(&req.game);
    if !share.files(&dir)?.iter().any(|n| n == "set.json") {
        return Ok(None);
    }
    let bytes = open_if_sealed(req, share.read(&format!("{dir}/set.json"))?)?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}

fn manifest_of(set: &StoredSet) -> Manifest {
    manifest::from_entries(set.files.iter().map(|f| f.entry.clone()).collect())
}

fn load_baseline(path: &Path) -> Option<Manifest> {
    let entries: Vec<FileEntry> = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    Some(manifest::from_entries(entries))
}

fn save_baseline(path: &Path, m: &Manifest) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let entries: Vec<&FileEntry> = m.values().collect();
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec(&entries)?)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn now_ms() -> i64 {
    crate::library::now_ms()
}

/// Syncs [`req`]'s game with [`share`]: the files on this device against the set on the share and
/// the baseline both last agreed on.
pub fn sync_with_share(share: &dyn Share, req: &ShareSaveRequest) -> Result<Outcome> {
    let baseline = load_baseline(req.baseline_path);
    let here = manifest::build(saves::collect(req.spec, req.roots), baseline.as_ref().unwrap_or(&Manifest::new()));
    let stored = read_set(share, req)?;
    let there = stored.as_ref().map(manifest_of).unwrap_or_default();
    let there_device = stored.as_ref().map(|s| s.device.clone()).unwrap_or_else(|| "the share".into());
    let plan = match (decide(&here, &there, baseline.as_ref()), req.choice) {
        // No set on the share is not "the share removed every file": this device's set goes there.
        _ if stored.is_none() && !here.is_empty() => copy_from(Side::Here, &here, &there, false),
        (Plan::Conflict, Some(side)) => copy_from(side, &here, &there, true),
        (Plan::Copy { from, .. }, Some(side)) if from != side => copy_from(side, &here, &there, true),
        (plan, _) => plan,
    };
    let outcome = match plan {
        Plan::Conflict => return Ok(Outcome::Conflict { here: save_set(req.name, &here), there: save_set(&there_device, &there) }),
        Plan::Nothing => Outcome::UpToDate { files: here.len() },
        Plan::Copy { from: Side::Here, changed, removed, keep } => {
            let bytes = push(share, req, &here, stored.as_ref(), keep)?;
            Outcome::Copied { from: Side::Here, files: changed.len(), removed: removed.len(), bytes, kept: keep }
        }
        Plan::Copy { from: Side::There, changed, removed, keep } => {
            let set = stored.as_ref().ok_or_else(|| protocol("the share has no saves to copy"))?;
            let bytes = pull(share, req, &here, set, &changed, &removed, keep)?;
            Outcome::Copied { from: Side::There, files: changed.len(), removed: removed.len(), bytes, kept: keep }
        }
    };
    // Both sides now hold the side copied from, or already matched.
    let settled = match &outcome {
        Outcome::Copied { from: Side::There, .. } => there,
        _ => here,
    };
    save_baseline(req.baseline_path, &settled)?;
    Ok(outcome)
}

/// Writes this device's set to the share: each file's blob unless the share already has it, then the
/// set itself in one step. When the share's set had changed too ([`keep`]) it is kept under `sets/`
/// first.
fn push(share: &dyn Share, req: &ShareSaveRequest, here: &Manifest, stored: Option<&StoredSet>, keep: bool) -> Result<u64> {
    let dir = game_dir(&req.game);
    let blob_dir = format!("{dir}/blobs");
    share.make_dirs(&blob_dir)?;
    let existing: std::collections::BTreeSet<String> = share.files(&blob_dir)?.into_iter().collect();
    // A file whose contents the share's set already holds keeps that blob.
    let known: std::collections::BTreeMap<&str, &str> =
        stored.map(|s| s.files.iter().map(|f| (f.entry.sha256.as_str(), f.blob.as_str())).collect()).unwrap_or_default();
    let mut files = Vec::new();
    let mut bytes = 0;
    for entry in here.values() {
        if let Some(blob) = known.get(entry.sha256.as_str()).filter(|b| existing.contains(**b)) {
            files.push(StoredFile { entry: entry.clone(), blob: blob.to_string() });
            continue;
        }
        let path = saves::local_path(&entry.name, req.roots).ok_or_else(|| protocol(format!("no place for {}", entry.name)))?;
        let plain = fs::read(&path)?;
        if hex_digest(&plain) != entry.sha256 {
            return Err(protocol(format!("{} changed while it was being sent; sync again", entry.name)));
        }
        let stored_bytes = seal_if(req, plain)?;
        let blob = hex_digest(&stored_bytes);
        if !existing.contains(&blob) {
            share.write(&format!("{blob_dir}/{blob}"), &stored_bytes)?;
        }
        bytes += entry.size;
        files.push(StoredFile { entry: entry.clone(), blob });
    }
    if keep {
        if let Some(old) = stored {
            share.make_dirs(&format!("{dir}/sets"))?;
            let device: String = old.device.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
            let name = format!("{dir}/sets/{}-{device}.json", utc_stamp(SystemTime::now()));
            share.write(&name, &seal_if(req, serde_json::to_vec(old)?)?)?;
        }
    }
    let set = StoredSet { device: req.name.to_string(), written_ms: now_ms(), files };
    share.write(&format!("{dir}/set.json"), &seal_if(req, serde_json::to_vec(&set)?)?)?;
    Ok(bytes)
}

/// Copies the share's set here: this device's own changed set is kept as its copy first ([`keep`]),
/// each changed file is fetched, checked against its digest and written under a temporary name, and
/// files the share's set no longer has are removed here.
fn pull(
    share: &dyn Share,
    req: &ShareSaveRequest,
    here: &Manifest,
    set: &StoredSet,
    changed: &[FileEntry],
    removed: &[String],
    keep: bool,
) -> Result<u64> {
    if keep {
        keep_copy(here.values().filter_map(|e| Some((e.name.clone(), saves::local_path(&e.name, req.roots)?))), req.archive_dir, req.name)?;
    }
    let dir = game_dir(&req.game);
    let blobs: std::collections::BTreeMap<String, &str> =
        set.files.iter().map(|f| (manifest::key(&f.entry.name), f.blob.as_str())).collect();
    let mut bytes = 0;
    for entry in changed {
        if !req.spec.matches(&entry.name) {
            return Err(protocol(format!("the share's set names {}, which is not one of this game's save files", entry.name)));
        }
        let blob =
            blobs.get(&manifest::key(&entry.name)).ok_or_else(|| protocol(format!("the share's set has no blob for {}", entry.name)))?;
        let plain = open_if_sealed(req, share.read(&format!("{dir}/blobs/{blob}"))?)?;
        if hex_digest(&plain) != entry.sha256 {
            return Err(protocol(format!("{} on the share is damaged", entry.name)));
        }
        let target: PathBuf = saves::local_path(&entry.name, req.roots).ok_or_else(|| protocol(format!("no place for {}", entry.name)))?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let file_name = target.file_name().and_then(|n| n.to_str()).unwrap_or("save").to_string();
        let tmp = target.with_file_name(format!(".{file_name}.part"));
        fs::write(&tmp, &plain)?;
        fs::rename(&tmp, &target)?;
        bytes += entry.size;
    }
    for name in removed {
        if !req.spec.matches(name) {
            continue;
        }
        if let Some(path) = saves::local_path(name, req.roots) {
            if let Err(e) = fs::remove_file(&path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    return Err(e.into());
                }
            }
        }
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::share::Folder;

    struct Device {
        root: PathBuf,
        ring: Keyring,
        name: &'static str,
    }

    impl Device {
        fn new(tag: &str, name: &'static str) -> Device {
            let root = std::env::temp_dir().join(format!("dta-sharesaves-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("saves")).unwrap();
            let mut ring = Keyring::new();
            ring.generate();
            Device { root, ring, name }
        }

        fn roots(&self) -> Roots {
            Roots::from([("<base>".to_string(), self.root.join("saves"))])
        }

        fn sync(&self, share: &Folder, sealed: bool, choice: Option<Side>) -> Result<Outcome> {
            let roots = self.roots();
            let spec = SaveSpec { patterns: vec!["<base>".into()] };
            let req = ShareSaveRequest {
                game: GameRef { key: "steam:1".into(), title: "Game".into() },
                spec: &spec,
                roots: &roots,
                baseline_path: &self.root.join("baseline.json"),
                archive_dir: &self.root.join("archive"),
                name: self.name,
                choice,
                lock: sealed.then_some(&self.ring),
            };
            sync_with_share(share, &req)
        }

        fn write(&self, name: &str, body: &str) {
            fs::write(self.root.join("saves").join(name), body).unwrap();
        }

        fn read(&self, name: &str) -> Option<String> {
            fs::read_to_string(self.root.join("saves").join(name)).ok()
        }
    }

    fn share(tag: &str) -> Folder {
        let root = std::env::temp_dir().join(format!("dta-sharesaves-share-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        Folder::new(root)
    }

    #[test]
    fn two_devices_keep_each_others_saves_through_a_share() {
        let share = share("plain");
        let a = Device::new("a1", "Handheld");
        let b = Device::new("b1", "Tablet");
        a.write("slot1.sav", "a's first");
        assert!(matches!(a.sync(&share, false, None).unwrap(), Outcome::Copied { from: Side::Here, files: 1, .. }));
        // The other device has none yet: it takes the share's.
        assert!(matches!(b.sync(&share, false, None).unwrap(), Outcome::Copied { from: Side::There, files: 1, .. }));
        assert_eq!(b.read("slot1.sav").as_deref(), Some("a's first"));
        // Nothing changed: nothing moves.
        assert!(matches!(a.sync(&share, false, None).unwrap(), Outcome::UpToDate { files: 1 }));
        // One side changes and wins.
        b.write("slot1.sav", "b's second");
        b.sync(&share, false, None).unwrap();
        a.sync(&share, false, None).unwrap();
        assert_eq!(a.read("slot1.sav").as_deref(), Some("b's second"));
    }

    #[test]
    fn both_changed_is_a_conflict_and_nobody_loses_saves() {
        let share = share("conflict");
        let a = Device::new("a2", "Handheld");
        let b = Device::new("b2", "Tablet");
        a.write("slot1.sav", "base");
        a.sync(&share, false, None).unwrap();
        b.sync(&share, false, None).unwrap();
        a.write("slot1.sav", "a changed");
        a.sync(&share, false, None).unwrap();
        b.write("slot1.sav", "b changed");
        assert!(matches!(b.sync(&share, false, None).unwrap(), Outcome::Conflict { .. }));
        assert_eq!(b.read("slot1.sav").as_deref(), Some("b changed"), "a conflict changes nothing");
        // The person keeps the share's: this device's set is kept as its copy first.
        assert!(matches!(b.sync(&share, false, Some(Side::There)).unwrap(), Outcome::Copied { kept: true, .. }));
        assert_eq!(b.read("slot1.sav").as_deref(), Some("a changed"));
        assert!(b.root.join("archive").join("copies").join("Tablet").is_dir());
    }

    #[test]
    fn an_emptied_share_never_deletes_this_devices_saves() {
        let share = share("emptied");
        let a = Device::new("a4", "Handheld");
        a.write("slot1.sav", "progress");
        a.sync(&share, false, None).unwrap();
        // The share's folder is gone (another share picked, or emptied by hand).
        fs::remove_dir_all(share.root()).unwrap();
        assert!(matches!(a.sync(&share, false, None).unwrap(), Outcome::Copied { from: Side::Here, .. }));
        assert_eq!(a.read("slot1.sav").as_deref(), Some("progress"));
    }

    #[test]
    fn a_locked_share_holds_age_files_only_and_any_device_with_the_key_opens_it() {
        let share = share("sealed");
        let a = Device::new("a3", "Handheld");
        a.write("slot1.sav", "secret progress");
        a.sync(&share, true, None).unwrap();
        // Nothing on the share reads as the save or its set.
        let dir = share.local_path(&game_dir(&GameRef { key: "steam:1".into(), title: "Game".into() })).unwrap();
        for entry in walk(&dir) {
            let bytes = fs::read(&entry).unwrap();
            assert!(locked::is_locked(&bytes), "{} is not an age file", entry.display());
            assert!(!String::from_utf8_lossy(&bytes).contains("secret progress"));
        }
        // Its own device reads it back after losing its files.
        fs::remove_file(a.root.join("saves").join("slot1.sav")).unwrap();
        fs::remove_file(a.root.join("baseline.json")).unwrap();
        a.sync(&share, true, None).unwrap();
        assert_eq!(a.read("slot1.sav").as_deref(), Some("secret progress"));
        // Another device can't open it without the key, and changes nothing.
        let mut b = Device::new("b3", "Tablet");
        assert!(b.sync(&share, true, None).is_err());
        assert!(b.read("slot1.sav").is_none());
        // Once the rings are merged (as a paired sync or an imported key does), it opens there too.
        b.ring.merge(&a.ring);
        assert!(matches!(b.sync(&share, true, None).unwrap(), Outcome::Copied { from: Side::There, .. }));
        assert_eq!(b.read("slot1.sav").as_deref(), Some("secret progress"));
        // And plain age opens a blob with the exported key file, as `age -d -i` does, without droidtop.
        let blob = walk(&dir).into_iter().find(|p| p.parent().is_some_and(|d| d.ends_with("blobs"))).unwrap();
        let exported = a.ring.to_text();
        let identity: age::x25519::Identity = age::secrecy::ExposeSecret::expose_secret(&exported)
            .lines()
            .find(|l| l.starts_with("AGE-SECRET-KEY-1"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(age::decrypt(&identity, &fs::read(blob).unwrap()).unwrap(), b"secret progress");
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push(path);
            }
        }
        out
    }
}
