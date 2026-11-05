//! Where store and forward leaves its letters: a share (docs/DESIGN.md section 10, transport 3;
//! Droidtop/tracker#573, #800 item 3).
//!
//! lib-sync controls synchronization; the share itself is a network resource that lives elsewhere
//! (owner, 2026-11-05, on #573: "A share can be a backend for a sync, but it's really just a network
//! resource we can use for lots of stuff"). So sync only names the few things it needs from one, in
//! [`Share`], and a share client (lib-net's `net-remote`: WebDAV and the backends to come) is one
//! implementation of it. [`Folder`] is the other: a folder on this computer that the person's own sync
//! tool carries.
//!
//! Paths are relative to the share's root and `/`-separated; [`check`] refuses anything that could
//! climb out of it.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use rand_core::{OsRng, RngCore};

/// What store and forward needs from a share.
pub trait Share: Send + Sync {
    /// The names of the files directly in [`dir`] (not folders, not files still being written). Empty
    /// when the folder is not there.
    fn files(&self, dir: &str) -> io::Result<Vec<String>>;

    /// [`path`]'s whole contents.
    fn read(&self, path: &str) -> io::Result<Vec<u8>>;

    /// Writes [`bytes`] to [`path`] in one step, so a reader or a sync tool never takes half of it. The
    /// folder must exist ([`Share::make_dirs`]).
    fn write(&self, path: &str, bytes: &[u8]) -> io::Result<()>;

    /// Removes the file [`path`]; nothing when it is already gone.
    fn remove(&self, path: &str) -> io::Result<()>;

    /// Makes [`dir`] and the folders above it.
    fn make_dirs(&self, dir: &str) -> io::Result<()>;
}

/// [`path`] when it is a plain path inside a share: `/`-separated, with no `..`, `.`, empty segment,
/// backslash or NUL. A leading or trailing `/` is dropped.
pub fn check(path: &str) -> io::Result<&str> {
    let trimmed = path.trim_matches('/');
    let bad = trimmed.is_empty()
        || trimmed.contains('\\')
        || trimmed.contains('\0')
        || trimmed.split('/').any(|s| s.is_empty() || s == "." || s == "..");
    if bad {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{path} is not a path inside the share")));
    }
    Ok(trimmed)
}

/// Whether [`name`] is a file a share is still writing: it starts with a dot and ends in `.part`, as
/// [`Folder::write`] and lib-net's share client name them.
pub fn is_partial(name: &str) -> bool {
    name.starts_with('.') && name.ends_with(".part")
}

/// A folder on this computer as a share.
#[derive(Debug, Clone)]
pub struct Folder {
    root: PathBuf,
}

impl Folder {
    pub fn new(root: impl Into<PathBuf>) -> Folder {
        Folder { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where [`path`] is on this computer.
    pub fn local_path(&self, path: &str) -> io::Result<PathBuf> {
        Ok(check(path)?.split('/').fold(self.root.clone(), |p, s| p.join(s)))
    }
}

impl Share for Folder {
    fn files(&self, dir: &str) -> io::Result<Vec<String>> {
        let read = match fs::read_dir(self.local_path(dir)?) {
            Ok(read) => read,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut names = Vec::new();
        for entry in read.flatten() {
            let Ok(name) = entry.file_name().into_string() else { continue };
            if !is_partial(&name) && entry.file_type().is_ok_and(|t| t.is_file()) {
                names.push(name);
            }
        }
        Ok(names)
    }

    fn read(&self, path: &str) -> io::Result<Vec<u8>> {
        fs::read(self.local_path(path)?)
    }

    fn write(&self, path: &str, bytes: &[u8]) -> io::Result<()> {
        let target = self.local_path(path)?;
        let name = target.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        let mut tag = [0u8; 6];
        OsRng.fill_bytes(&mut tag);
        let tmp = target.with_file_name(format!(".{name}.{}.part", crate::hex::encode(&tag)));
        let written = (|| {
            let mut file = File::create(&tmp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            fs::rename(&tmp, &target)
        })();
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        written
    }

    fn remove(&self, path: &str) -> io::Result<()> {
        match fs::remove_file(self.local_path(path)?) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    fn make_dirs(&self, dir: &str) -> io::Result<()> {
        fs::create_dir_all(self.local_path(dir)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_inside_the_share() {
        assert_eq!(check("/a/b/").unwrap(), "a/b");
        for bad in ["", "/", "a/../b", "..", "a/./b", "a//b", "a\\b", "a\0b"] {
            assert!(check(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_folder_writes_in_one_step_and_lists_only_finished_files() {
        let root = std::env::temp_dir().join(format!("dta-share-folder-{}", std::process::id()));
        let share = Folder::new(&root);
        share.make_dirs("a/b").unwrap();
        share.write("a/b/one.dtmsg", b"one").unwrap();
        share.write("a/b/one.dtmsg", b"two").unwrap();
        fs::write(root.join("a/b/.half.dtmsg.abc.part"), b"half").unwrap();
        share.make_dirs("a/b/sub").unwrap();
        assert_eq!(share.read("a/b/one.dtmsg").unwrap(), b"two");
        assert_eq!(share.files("a/b").unwrap(), vec!["one.dtmsg".to_string()]);
        assert!(share.files("a/none").unwrap().is_empty());
        share.remove("a/b/one.dtmsg").unwrap();
        share.remove("a/b/one.dtmsg").unwrap();
        assert!(share.files("a/b").unwrap().is_empty());
        assert!(share.write("../out", b"x").is_err());
        let _ = fs::remove_dir_all(&root);
    }
}
