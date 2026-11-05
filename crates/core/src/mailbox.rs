//! Store and forward through the person's own cloud share (docs/DESIGN.md
//! section 10, transport 3). A share is a folder the person's own sync tool
//! carries to both devices. Each device leaves sealed messages for a peer in
//! `droidtop-agent/<peer id>/inbox/`; the peer opens, applies and removes
//! them when they arrive. The share only ever holds ciphertext.
//!
//! The share is any [`Remote`]: a folder on this computer ([`LocalFolder`], what [`post`] and
//! [`collect`] use) or a WebDAV share and the other backends of sync-remote ([`post_to`],
//! [`collect_from`]; Droidtop/tracker#800 item 3, #573).
//!
//! Sealing: XChaCha20-Poly1305 under a key derived from X25519 between the
//! two identities, a random 24-byte nonce per message, and the sender, the
//! recipient and the format in the associated data.

use std::io;
use std::path::{Path, PathBuf};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};

use serde::{Deserialize, Serialize};
use sync_remote::{is_temporary, LocalFolder, Remote, RemotePath};

use crate::keys::{short, DeviceKey, PeerId};
use crate::library::Change;
use crate::manifest::FileEntry;
use crate::proto::GameRef;
use crate::{error::protocol, Error, Result};

const MAGIC: &[u8; 4] = b"DTM1";
const EXT: &str = "dtmsg";

fn box_key(key: &DeviceKey, peer: &PeerId) -> Result<[u8; 32]> {
    let shared = key.shared_secret(peer)?;
    let mut h = Sha256::new();
    h.update(b"droidtop-agent mailbox v1");
    h.update(shared);
    Ok(h.finalize().into())
}

fn aad(from: &PeerId, to: &PeerId) -> Vec<u8> {
    let mut a = MAGIC.to_vec();
    a.extend_from_slice(&from.0);
    a.extend_from_slice(&to.0);
    a
}

/// [`payload`] sealed from [`key`]'s device to [`to`].
pub fn seal(key: &DeviceKey, to: &PeerId, payload: &[u8]) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(&box_key(key, to)?.into());
    let mut nonce = [0u8; 24];
    OsRng.fill_bytes(&mut nonce);
    let from = key.peer_id();
    let sealed = cipher
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: payload, aad: &aad(&from, to) })
        .map_err(|_| protocol("a message could not be sealed"))?;
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&from.0);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&sealed);
    Ok(out)
}

/// The sender and payload of a message sealed for [`key`]'s device, when the
/// sender is one [`trusted`] accepts.
pub fn open(key: &DeviceKey, bytes: &[u8], trusted: impl Fn(&PeerId) -> bool) -> Result<(PeerId, Vec<u8>)> {
    if bytes.len() < 4 + 32 + 24 + 16 || &bytes[..4] != MAGIC {
        return Err(protocol("not a droidtop-agent message"));
    }
    let from = PeerId(bytes[4..36].try_into().expect("32 bytes"));
    if !trusted(&from) {
        return Err(Error::NotPaired);
    }
    let cipher = XChaCha20Poly1305::new(&box_key(key, &from)?.into());
    let payload = cipher
        .decrypt(XNonce::from_slice(&bytes[36..60]), Payload { msg: &bytes[60..], aad: &aad(&from, &key.peer_id()) })
        .map_err(|_| protocol("a message was damaged or not for this device"))?;
    Ok((from, payload))
}

/// The folder messages for [`peer`] go to under [`share`].
pub fn inbox(share: &Path, peer: &PeerId) -> PathBuf {
    share.join("droidtop-agent").join(peer.to_hex()).join("inbox")
}

/// The folder messages for [`peer`] go to inside a share.
pub fn inbox_path(peer: &PeerId) -> RemotePath {
    RemotePath::new(&format!("droidtop-agent/{}/inbox", peer.to_hex())).expect("a hex id is a plain name")
}

/// Leaves a sealed message for [`to`] in the folder [`share`] ([`post_to`]).
pub fn post(share: &Path, key: &DeviceKey, to: &PeerId, payload: &[u8]) -> Result<PathBuf> {
    let folder = LocalFolder::new(share);
    let posted = post_to(&folder, key, to, payload)?;
    Ok(folder.local_path(&posted))
}

/// Leaves a sealed message for [`to`] in [`share`]. The share writes it in one step (a temporary
/// name, then renamed), so a sync tool or a reader never takes half of it.
pub fn post_to(share: &dyn Remote, key: &DeviceKey, to: &PeerId, payload: &[u8]) -> Result<RemotePath> {
    let dir = inbox_path(to);
    share.make_dirs(&dir).map_err(io::Error::from)?;
    let mut tag = [0u8; 6];
    OsRng.fill_bytes(&mut tag);
    let name = format!("{}-{}-{}.{EXT}", short(&key.peer_id()), crate::library::now_ms(), crate::hex::encode(&tag));
    let path = dir.join(&name).map_err(io::Error::from)?;
    share.write(&path, &seal(key, to, payload)?).map_err(io::Error::from)?;
    Ok(path)
}

/// What a message carries. File contents follow the header in [`pack`]'s
/// layout, in the order the header lists them.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Envelope {
    /// Library changes the recipient has not had yet.
    Library { changes: Vec<Change> },
    /// A game's whole save set, made when no live path was there, with the
    /// baseline the sender last agreed on with the recipient. When the
    /// recipient's saves changed too, nobody wins: the recipient changes
    /// nothing and keeps the set as the sender's copy, and the next live
    /// sync asks the person.
    Saves { game: GameRef, base: Vec<FileEntry>, files: Vec<FileEntry> },
    /// The recipient could not apply a save set, and why.
    SavesRefused { game: GameRef, reason: String },
}

/// An envelope and its file contents as one payload: a 4-byte header length,
/// the JSON header, then the contents back to back.
pub fn pack(envelope: &Envelope, contents: &[Vec<u8>]) -> Result<Vec<u8>> {
    let header = serde_json::to_vec(envelope)?;
    let mut out = (header.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(&header);
    for c in contents {
        out.extend_from_slice(c);
    }
    Ok(out)
}

/// The envelope and the bytes after its header.
pub fn unpack(payload: &[u8]) -> Result<(Envelope, &[u8])> {
    if payload.len() < 4 {
        return Err(protocol("an empty message"));
    }
    let len = u32::from_be_bytes(payload[..4].try_into().expect("4 bytes")) as usize;
    let header = payload.get(4..4 + len).ok_or_else(|| protocol("a truncated message"))?;
    Ok((serde_json::from_slice(header)?, &payload[4 + len..]))
}

/// The letters opened, and the files left unopened with the reason.
pub type Collected = (Vec<Letter>, Vec<(PathBuf, String)>);

/// One message waiting in this device's inbox.
pub struct Letter {
    pub from: PeerId,
    pub payload: Vec<u8>,
    pub path: PathBuf,
}

/// The messages waiting for [`key`]'s device in the folder [`share`], oldest first ([`collect_from`]).
pub fn collect(share: &Path, key: &DeviceKey, trusted: impl Fn(&PeerId) -> bool) -> io::Result<Collected> {
    let folder = LocalFolder::new(share);
    let (letters, failed) = collect_from(&folder, key, trusted)?;
    Ok((
        letters.into_iter().map(|l| Letter { from: l.from, payload: l.payload, path: folder.local_path(&l.path) }).collect(),
        failed.into_iter().map(|(p, why)| (folder.local_path(&p), why)).collect(),
    ))
}

/// The letters opened from a share, and the files left unopened with the reason.
pub type CollectedFrom = (Vec<ShareLetter>, Vec<(RemotePath, String)>);

/// One message waiting in this device's inbox on a share; the caller removes [`path`] once it applied it.
pub struct ShareLetter {
    pub from: PeerId,
    pub payload: Vec<u8>,
    pub path: RemotePath,
}

/// The messages waiting for [`key`]'s device in [`share`], oldest first.
/// Messages it cannot open are left where they are (a sync tool may still
/// be writing them) and reported in the second list.
pub fn collect_from(share: &dyn Remote, key: &DeviceKey, trusted: impl Fn(&PeerId) -> bool) -> io::Result<CollectedFrom> {
    let dir = inbox_path(&key.peer_id());
    let mut names: Vec<String> = match share.list(&dir) {
        Ok(entries) => entries
            .into_iter()
            .filter(|e| !e.is_dir && !is_temporary(&e.name) && e.name.ends_with(&format!(".{EXT}")))
            .map(|e| e.name)
            .collect(),
        Err(sync_remote::Error::NotFound(_)) => return Ok((Vec::new(), Vec::new())),
        Err(e) => return Err(e.into()),
    };
    names.sort();
    let mut letters = Vec::new();
    let mut failed = Vec::new();
    for name in names {
        let path = dir.join(&name).map_err(io::Error::from)?;
        let bytes = share.read(&path)?;
        match open(key, &bytes, &trusted) {
            Ok((from, payload)) => letters.push(ShareLetter { from, payload, path }),
            Err(e) => failed.push((path, e.to_string())),
        }
    }
    Ok((letters, failed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn sealed_messages_open_only_for_their_recipient() {
        let a = DeviceKey::generate();
        let b = DeviceKey::generate();
        let c = DeviceKey::generate();
        let sealed = seal(&a, &b.peer_id(), b"saves").unwrap();
        let (from, payload) = open(&b, &sealed, |_| true).unwrap();
        assert_eq!(from, a.peer_id());
        assert_eq!(payload, b"saves");
        assert!(open(&c, &sealed, |_| true).is_err());
        assert!(matches!(open(&b, &sealed, |_| false), Err(Error::NotPaired)));
        let mut tampered = sealed.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(open(&b, &tampered, |_| true).is_err());
    }

    #[test]
    fn posted_letters_are_collected() {
        let share = std::env::temp_dir().join(format!("dta-share-{}", std::process::id()));
        let a = DeviceKey::generate();
        let b = DeviceKey::generate();
        post(&share, &a, &b.peer_id(), b"one").unwrap();
        post(&share, &a, &b.peer_id(), b"two").unwrap();
        let (letters, failed) = collect(&share, &b, |p| *p == a.peer_id()).unwrap();
        assert!(failed.is_empty());
        let mut got: Vec<Vec<u8>> = letters.into_iter().map(|l| l.payload).collect();
        got.sort();
        assert_eq!(got, vec![b"one".to_vec(), b"two".to_vec()]);
        let _ = fs::remove_dir_all(&share);
    }

    #[test]
    fn letters_on_any_share_are_collected_and_half_written_ones_are_not() {
        let root = std::env::temp_dir().join(format!("dta-remote-share-{}", std::process::id()));
        let share = LocalFolder::new(&root);
        let a = DeviceKey::generate();
        let b = DeviceKey::generate();
        let posted = post_to(&share, &a, &b.peer_id(), b"one").unwrap();
        assert_eq!(posted.parent().unwrap(), inbox_path(&b.peer_id()));
        // A file a sync tool is still writing, and one this device can't open.
        share.write(&inbox_path(&b.peer_id()).join(".x.dtmsg.abc.part").unwrap(), b"half").unwrap();
        share.write(&inbox_path(&b.peer_id()).join("junk.dtmsg").unwrap(), b"junk").unwrap();
        let (letters, failed) = collect_from(&share, &b, |p| *p == a.peer_id()).unwrap();
        assert_eq!(letters.len(), 1);
        assert_eq!(letters[0].payload, b"one");
        assert_eq!(letters[0].path, posted);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].0.name(), "junk.dtmsg");
        // Nothing for a device with no inbox yet.
        let (none, _) = collect_from(&share, &a, |_| true).unwrap();
        assert!(none.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn envelopes_pack_with_their_contents() {
        let e = Envelope::SavesRefused { game: GameRef { key: "steam:1".into(), title: "G".into() }, reason: "busy".into() };
        let packed = pack(&e, &[b"abc".to_vec()]).unwrap();
        let (back, rest) = unpack(&packed).unwrap();
        assert_eq!(back, e);
        assert_eq!(rest, b"abc");
    }
}
