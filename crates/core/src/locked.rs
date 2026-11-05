//! What droidtop locks on the person's shares, in a format they can open themselves (Droidtop/tracker#573).
//!
//! The owner, 2026-11-05: "When parts of a share (since it doesn't have to be all or nothing) are encrypted, we
//! can sync those keys, and let the user export the key. We should use well-known crypto and encapsulation
//! mechanisms, so the user can retrieve that data themselves (for data that is tied to games)."
//!
//! - **The format is age** (age-encryption.org/v1): X25519 recipients, ChaCha20-Poly1305 in 64 KiB chunks. A
//!   locked file opens without droidtop with the age or rage tools: `age -d -i droidtop-share-key.txt file.age`.
//! - **One share key per person**, not per device, kept as a [`Keyring`]. Paired devices merge their rings as a
//!   union, so two keys made before two devices met both stay: a file is locked to the ring's newest key and opens
//!   with any key in it, and nothing ever becomes unreadable through a sync.
//! - **The ring's text is an age identity file**, the format `age-keygen` writes, with a comment line per key
//!   saying when droidtop made it. So an exported ring is the very file `age -d -i` takes.
//!   [`Keyring::export_with_passphrase`] locks that text with age's passphrase mode (scrypt) for an export the
//!   person keeps somewhere less safe.

use std::io::Read;

use age::secrecy::{ExposeSecret, SecretString};
use age::x25519;

use crate::{error::protocol, Result};

/// How every age file starts.
pub const AGE_MAGIC: &[u8] = b"age-encryption.org/v1\n";

/// The comment before each key in a ring's text: when droidtop made it, in ms since the Unix epoch.
const MADE: &str = "# droidtop share key, made ";

/// Whether [`bytes`] are an age file.
pub fn is_locked(bytes: &[u8]) -> bool {
    bytes.starts_with(AGE_MAGIC)
}

struct ShareKey {
    identity: x25519::Identity,
    /// When it was made, in ms since the Unix epoch; 0 when a key file did not say.
    made_ms: i64,
}

impl ShareKey {
    fn public(&self) -> String {
        self.identity.to_public().to_string()
    }
}

/// The person's share keys.
#[derive(Default)]
pub struct Keyring {
    keys: Vec<ShareKey>,
}

impl std::fmt::Debug for Keyring {
    // Only the public halves are ever printed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.keys.iter().map(|k| k.public())).finish()
    }
}

impl Keyring {
    pub fn new() -> Keyring {
        Keyring::default()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Makes a new key and adds it to the ring, where it becomes the one new files are locked to. Its public half.
    pub fn generate(&mut self) -> String {
        let key = ShareKey { identity: x25519::Identity::generate(), made_ms: crate::library::now_ms() };
        let public = key.public();
        self.keys.push(key);
        public
    }

    /// The public halves (`age1…`), newest first: what a file is locked to, and what a screen may show.
    pub fn public_keys(&self) -> Vec<String> {
        let mut keys: Vec<&ShareKey> = self.keys.iter().collect();
        keys.sort_by(|a, b| b.made_ms.cmp(&a.made_ms).then_with(|| b.public().cmp(&a.public())));
        keys.into_iter().map(ShareKey::public).collect()
    }

    /// The key new files are locked to: the newest, and among keys made at the same moment, the greatest public
    /// half, so every device that holds the same ring picks the same one.
    fn newest(&self) -> Option<&ShareKey> {
        self.keys.iter().max_by(|a, b| a.made_ms.cmp(&b.made_ms).then_with(|| a.public().cmp(&b.public())))
    }

    /// Adds the keys of [`other`] this ring lacks. Never removes one. How many it added.
    pub fn merge(&mut self, other: &Keyring) -> usize {
        let mut added = 0;
        for key in &other.keys {
            if !self.keys.iter().any(|k| k.public() == key.public()) {
                self.keys.push(ShareKey { identity: key.identity.clone(), made_ms: key.made_ms });
                added += 1;
            }
        }
        added
    }

    /// The ring as an age identity file: for the vault, for the sync to paired devices, and for an export.
    pub fn to_text(&self) -> SecretString {
        let mut text = String::new();
        for key in &self.keys {
            text.push_str(&format!("{MADE}{}\n# public key: {}\n", key.made_ms, key.public()));
            text.push_str(key.identity.to_string().expose_secret());
            text.push('\n');
        }
        SecretString::from(text)
    }

    /// A ring from an age identity file: droidtop's own text, or a file `age-keygen` made. Lines starting with `#`
    /// are comments, as age reads them; droidtop's comment says when a key was made.
    pub fn from_text(text: &str) -> Result<Keyring> {
        let mut ring = Keyring::new();
        let mut made_ms = 0;
        for line in text.lines().map(str::trim) {
            if let Some(ms) = line.strip_prefix(MADE) {
                made_ms = ms.trim().parse().unwrap_or(0);
                continue;
            }
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let identity: x25519::Identity = line.parse().map_err(|_| protocol("a line of the key file is not an age key"))?;
            let key = ShareKey { identity, made_ms };
            if !ring.keys.iter().any(|k| k.public() == key.public()) {
                ring.keys.push(key);
            }
            made_ms = 0;
        }
        if ring.is_empty() {
            return Err(protocol("the key file holds no age key"));
        }
        Ok(ring)
    }

    /// [`plain`] as an age file locked to the ring's newest key.
    pub fn lock(&self, plain: &[u8]) -> Result<Vec<u8>> {
        let key = self.newest().ok_or_else(|| protocol("there is no share key to lock with yet"))?;
        age::encrypt(&key.identity.to_public(), plain).map_err(|e| protocol(format!("could not lock: {e}")))
    }

    /// The contents of the age file [`locked`], opened with any key in the ring.
    pub fn open(&self, locked: &[u8]) -> Result<Vec<u8>> {
        let decryptor = age::Decryptor::new(locked).map_err(|e| protocol(format!("not an age file ({e})")))?;
        let mut reader = decryptor
            .decrypt(self.keys.iter().map(|k| &k.identity as &dyn age::Identity))
            .map_err(|_| protocol("this was locked with a share key this device doesn't have; import that key"))?;
        let mut plain = Vec::new();
        reader.read_to_end(&mut plain)?;
        Ok(plain)
    }

    /// The ring's text locked with [`passphrase`] in age's passphrase mode, for an export: `age -d` asks for it.
    pub fn export_with_passphrase(&self, passphrase: SecretString) -> Result<Vec<u8>> {
        let recipient = age::scrypt::Recipient::new(passphrase);
        age::encrypt(&recipient, self.to_text().expose_secret().as_bytes()).map_err(|e| protocol(format!("could not lock the export: {e}")))
    }

    /// A ring from an exported key file: plain text, or locked with a passphrase ([`passphrase`]).
    pub fn import(bytes: &[u8], passphrase: Option<SecretString>) -> Result<Keyring> {
        let text = if is_locked(bytes) {
            let passphrase = passphrase.ok_or_else(|| protocol("this key file is locked with a passphrase"))?;
            age::decrypt(&age::scrypt::Identity::new(passphrase), bytes)
                .map_err(|_| protocol("that passphrase doesn't open this key file"))?
        } else {
            bytes.to_vec()
        };
        let text = String::from_utf8(text).map_err(|_| protocol("the key file is not text"))?;
        Keyring::from_text(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_locked_file_is_a_standard_age_file_its_key_opens() {
        let mut ring = Keyring::new();
        ring.generate();
        let locked = ring.lock(b"slot 1").unwrap();
        assert!(is_locked(&locked));
        // What `age -d -i <exported key file>` does: read the identity file, decrypt with it.
        let text = ring.to_text();
        let identity: x25519::Identity = text.expose_secret().lines().find(|l| l.starts_with("AGE-SECRET-KEY-1")).unwrap().parse().unwrap();
        assert_eq!(age::decrypt(&identity, &locked).unwrap(), b"slot 1");
        assert_eq!(ring.open(&locked).unwrap(), b"slot 1");
    }

    #[test]
    fn rings_merge_as_a_union_and_every_key_still_opens() {
        let mut a = Keyring::new();
        a.generate();
        let mut b = Keyring::new();
        b.generate();
        let from_a = a.lock(b"a's").unwrap();
        let from_b = b.lock(b"b's").unwrap();
        assert!(a.open(&from_b).is_err(), "a ring without the key can't open it");
        assert_eq!(a.merge(&b), 1);
        assert_eq!(a.merge(&b), 0, "merging again adds nothing");
        assert_eq!(b.merge(&a), 1);
        assert_eq!(a.open(&from_b).unwrap(), b"b's");
        assert_eq!(b.open(&from_a).unwrap(), b"a's");
        // Both pick the same key to lock with.
        assert_eq!(a.public_keys()[0], b.public_keys()[0]);
    }

    #[test]
    fn a_ring_round_trips_through_its_text_and_a_passphrase_export() {
        let mut ring = Keyring::new();
        ring.generate();
        ring.generate();
        let back = Keyring::from_text(ring.to_text().expose_secret()).unwrap();
        assert_eq!(back.public_keys(), ring.public_keys());
        let export = ring.export_with_passphrase(SecretString::from("correct horse".to_string())).unwrap();
        assert!(is_locked(&export));
        assert!(Keyring::import(&export, None).is_err());
        assert!(Keyring::import(&export, Some(SecretString::from("wrong".to_string()))).is_err());
        let imported = Keyring::import(&export, Some(SecretString::from("correct horse".to_string()))).unwrap();
        assert_eq!(imported.public_keys(), ring.public_keys());
        // A key file made by age-keygen, with its own comments, imports too.
        let keygen = format!(
            "# created: 2026-11-05T10:00:00Z\n# public key: x\n{}\n",
            Keyring::from_text(ring.to_text().expose_secret()).unwrap().keys[0].identity.to_string().expose_secret()
        );
        assert_eq!(Keyring::import(keygen.as_bytes(), None).unwrap().len(), 1);
        assert!(Keyring::from_text("# nothing here\n").is_err());
        assert!(Keyring::from_text("not a key\n").is_err());
    }

    #[test]
    fn the_secret_halves_are_never_printed() {
        let mut ring = Keyring::new();
        ring.generate();
        assert!(!format!("{ring:?}").contains("AGE-SECRET-KEY"));
    }
}
