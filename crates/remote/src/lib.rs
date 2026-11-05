//! The person's own shares and cloud storage, behind one interface (Droidtop/tracker#573, design D1).
//!
//! Every user of a share goes through [`Remote`]: the store-and-forward mailbox, save and droidtop
//! backups, a games root and the file picker. A backend is one implementation of it:
//! - [`LocalFolder`]: a folder on this computer (one the person's own sync tool carries, or a mount);
//! - [`WebDav`]: Nextcloud, ownCloud, most NAS and any WebDAV server.
//!
//! Paths are relative to the share's root, `/`-separated, and checked by [`RemotePath`]: no `..`, no
//! `.`, no empty segment and no backslash, so nothing a caller builds can climb out of the share.
//!
//! Blocking I/O on purpose, as in sync-core: callers run it on their own background thread. Every
//! network backend has a timeout, so a share that does not answer is an [`Error::Unreachable`],
//! never a hang.

use std::fmt;
use std::io::{self, Read, Write};

mod local;
mod webdav;

pub use local::LocalFolder;
pub use webdav::{WebDav, WebDavConfig};

/// What went wrong, in words a person can read: droidtop shows these as the share's state.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The file or folder is not there.
    #[error("{0} is not on the share")]
    NotFound(String),
    /// The share answered and refused the sign-in.
    #[error("the share refused the sign-in")]
    SignInRefused,
    /// The share answered and refused this operation (permissions, a full disk, a locked file).
    #[error("the share refused this ({0})")]
    Refused(String),
    /// The share did not answer: offline, a wrong address, or a timeout.
    #[error("can't reach the share ({0})")]
    Unreachable(String),
    /// Plain HTTP was asked for without the person allowing it ([`WebDavConfig::allow_plain_http`]).
    #[error("the share's address is not encrypted (http); use https, or allow unencrypted for this share")]
    NotEncrypted,
    /// A path a caller built was not a plain path inside the share.
    #[error("{0} is not a path inside the share")]
    BadPath(String),
    /// The share answered with something this client cannot read.
    #[error("the share's answer could not be read ({0})")]
    Protocol(String),
    #[error("{0}")]
    Io(#[from] io::Error),
}

impl From<Error> for io::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Io(e) => e,
            Error::NotFound(_) => io::Error::new(io::ErrorKind::NotFound, e),
            Error::SignInRefused | Error::Refused(_) => io::Error::new(io::ErrorKind::PermissionDenied, e),
            Error::Unreachable(_) => io::Error::new(io::ErrorKind::TimedOut, e),
            other => io::Error::other(other),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// A checked path inside a share: `""` is its root.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RemotePath(String);

impl RemotePath {
    /// The share's root.
    pub fn root() -> RemotePath {
        RemotePath(String::new())
    }

    /// [`path`] when it is a plain path inside the share. A leading or trailing `/` is allowed and dropped.
    pub fn new(path: &str) -> Result<RemotePath> {
        let trimmed = path.trim_matches('/');
        if trimmed.is_empty() {
            return Ok(RemotePath::root());
        }
        let bad = trimmed.contains('\\') || trimmed.contains('\0') || trimmed.split('/').any(|s| s.is_empty() || s == "." || s == "..");
        if bad {
            return Err(Error::BadPath(path.to_string()));
        }
        Ok(RemotePath(trimmed.to_string()))
    }

    /// This path with one more segment, which must be a plain name.
    pub fn join(&self, name: &str) -> Result<RemotePath> {
        if name.is_empty() || name.contains('/') {
            return Err(Error::BadPath(name.to_string()));
        }
        RemotePath::new(&if self.0.is_empty() { name.to_string() } else { format!("{}/{name}", self.0) })
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// The folder this path is in, or None for the root.
    pub fn parent(&self) -> Option<RemotePath> {
        if self.0.is_empty() {
            return None;
        }
        Some(RemotePath(self.0.rsplit_once('/').map(|(p, _)| p.to_string()).unwrap_or_default()))
    }

    /// The last segment, or `""` for the root.
    pub fn name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or("")
    }

    /// Each segment in order.
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split('/').filter(|s| !s.is_empty())
    }
}

impl fmt::Display for RemotePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "/{}", self.0)
    }
}

impl fmt::Debug for RemotePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RemotePath({:?})", self.0)
    }
}

/// One file or folder in a share.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    /// 0 for a folder.
    pub size: u64,
    /// The modification time, in ms since the Unix epoch; 0 when the share does not say.
    pub mtime_ms: i64,
}

impl Entry {
    pub fn new(name: impl Into<String>, is_dir: bool, size: u64, mtime_ms: i64) -> Entry {
        Entry { name: name.into(), is_dir, size, mtime_ms }
    }
}

/// A share: the one interface every user of a share goes through (module docs).
pub trait Remote: Send + Sync {
    /// What is directly in [`dir`], in no particular order. [`Error::NotFound`] when the folder is not there.
    fn list(&self, dir: &RemotePath) -> Result<Vec<Entry>>;

    /// [`path`]'s entry, or None when it is not there.
    fn stat(&self, path: &RemotePath) -> Result<Option<Entry>>;

    /// Copies [`path`]'s contents into [`sink`]; the number of bytes.
    fn read_into(&self, path: &RemotePath, sink: &mut dyn Write) -> Result<u64>;

    /// Up to [`len`] bytes of [`path`] from [`offset`] (fewer at its end).
    fn read_range(&self, path: &RemotePath, offset: u64, len: u64) -> Result<Vec<u8>>;

    /// Writes [`path`] from [`source`], replacing what was there, in one step: the contents go to a
    /// temporary name first and are renamed onto [`path`], so nothing ever sees half a file. The folder
    /// must exist ([`Remote::make_dirs`]). The number of bytes.
    fn write_from(&self, path: &RemotePath, source: &mut dyn Read) -> Result<u64>;

    /// Moves [`from`] onto [`to`], replacing a file there.
    fn rename(&self, from: &RemotePath, to: &RemotePath) -> Result<()>;

    /// Removes the file [`path`]; nothing when it is already gone. Never removes a folder's contents.
    fn remove(&self, path: &RemotePath) -> Result<()>;

    /// Makes [`dir`] and the folders above it; nothing for those already there.
    fn make_dirs(&self, dir: &RemotePath) -> Result<()>;

    /// [`path`]'s whole contents.
    fn read(&self, path: &RemotePath) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        self.read_into(path, &mut out)?;
        Ok(out)
    }

    /// Writes [`bytes`] to [`path`] in one step ([`Remote::write_from`]).
    fn write(&self, path: &RemotePath, bytes: &[u8]) -> Result<()> {
        self.write_from(path, &mut &bytes[..]).map(|_| ())
    }
}

/// A random name for a file being written, next to [`path`], that no reader takes for a real one: it
/// starts with a dot and ends in `.part`.
pub(crate) fn temporary_name(path: &RemotePath) -> Result<RemotePath> {
    use rand_core::{OsRng, RngCore};
    let mut tag = [0u8; 6];
    OsRng.fill_bytes(&mut tag);
    let tag: String = tag.iter().map(|b| format!("{b:02x}")).collect();
    let name = format!(".{}.{tag}.part", path.name());
    match path.parent() {
        Some(parent) => parent.join(&name),
        None => Err(Error::BadPath(path.to_string())),
    }
}

/// Whether [`name`] is a file [`Remote::write_from`] is still writing.
pub fn is_temporary(name: &str) -> bool {
    name.starts_with('.') && name.ends_with(".part")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_inside_the_share() {
        assert_eq!(RemotePath::new("/a/b/").unwrap().as_str(), "a/b");
        assert!(RemotePath::new("").unwrap().is_root());
        assert!(RemotePath::new("/").unwrap().is_root());
        for bad in ["a/../b", "..", "a/./b", "a//b", "a\\b", "a\0b"] {
            assert!(matches!(RemotePath::new(bad), Err(Error::BadPath(_))), "{bad}");
        }
        let a = RemotePath::new("a").unwrap();
        assert!(a.join("b/c").is_err());
        assert!(a.join("..").is_err());
        assert!(a.join("").is_err());
        assert_eq!(a.join("b").unwrap().as_str(), "a/b");
        assert_eq!(RemotePath::root().join("x").unwrap().as_str(), "x");
    }

    #[test]
    fn parents_and_names() {
        let p = RemotePath::new("a/b/c.txt").unwrap();
        assert_eq!(p.name(), "c.txt");
        assert_eq!(p.parent().unwrap().as_str(), "a/b");
        assert!(RemotePath::new("x").unwrap().parent().unwrap().is_root());
        assert!(RemotePath::root().parent().is_none());
        assert_eq!(p.segments().collect::<Vec<_>>(), vec!["a", "b", "c.txt"]);
    }

    #[test]
    fn temporary_names_sit_beside_their_file() {
        let t = temporary_name(&RemotePath::new("a/b.txt").unwrap()).unwrap();
        assert_eq!(t.parent().unwrap().as_str(), "a");
        assert!(is_temporary(t.name()), "{t}");
        assert!(!is_temporary("b.txt"));
        assert!(temporary_name(&RemotePath::root()).is_err());
    }
}
