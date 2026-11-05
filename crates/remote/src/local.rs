//! A folder on this computer as a share: one the person's own sync tool carries to their other
//! devices, or a share the system has mounted.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::{temporary_name, Entry, Error, Remote, RemotePath, Result};

/// A folder on this computer, read and written with the file system.
#[derive(Debug, Clone)]
pub struct LocalFolder {
    root: PathBuf,
}

impl LocalFolder {
    pub fn new(root: impl Into<PathBuf>) -> LocalFolder {
        LocalFolder { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where [`path`] is on this computer.
    pub fn local_path(&self, path: &RemotePath) -> PathBuf {
        path.segments().fold(self.root.clone(), |p, s| p.join(s))
    }
}

fn not_found(path: &RemotePath) -> impl FnOnce(io::Error) -> Error + '_ {
    move |e| if e.kind() == io::ErrorKind::NotFound { Error::NotFound(path.to_string()) } else { Error::Io(e) }
}

fn entry_of(name: String, meta: &fs::Metadata) -> Entry {
    let mtime_ms = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64).unwrap_or(0);
    let is_dir = meta.is_dir();
    Entry::new(name, is_dir, if is_dir { 0 } else { meta.len() }, mtime_ms)
}

impl Remote for LocalFolder {
    fn list(&self, dir: &RemotePath) -> Result<Vec<Entry>> {
        let mut out = Vec::new();
        for item in fs::read_dir(self.local_path(dir)).map_err(not_found(dir))? {
            let item = item?;
            let Ok(name) = item.file_name().into_string() else { continue };
            // A name the share's path rules refuse can't be addressed through this interface.
            if RemotePath::root().join(&name).is_err() {
                continue;
            }
            out.push(entry_of(name, &item.metadata()?));
        }
        Ok(out)
    }

    fn stat(&self, path: &RemotePath) -> Result<Option<Entry>> {
        match fs::metadata(self.local_path(path)) {
            Ok(meta) => Ok(Some(entry_of(path.name().to_string(), &meta))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn read_into(&self, path: &RemotePath, sink: &mut dyn Write) -> Result<u64> {
        let mut file = File::open(self.local_path(path)).map_err(not_found(path))?;
        Ok(io::copy(&mut file, sink)?)
    }

    fn read_range(&self, path: &RemotePath, offset: u64, len: u64) -> Result<Vec<u8>> {
        let mut file = File::open(self.local_path(path)).map_err(not_found(path))?;
        file.seek(SeekFrom::Start(offset))?;
        let mut out = Vec::new();
        file.take(len).read_to_end(&mut out)?;
        Ok(out)
    }

    fn write_from(&self, path: &RemotePath, source: &mut dyn Read) -> Result<u64> {
        let target = self.local_path(path);
        let tmp = self.local_path(&temporary_name(path)?);
        let written = (|| -> io::Result<u64> {
            let mut file = File::create(&tmp)?;
            let n = io::copy(source, &mut file)?;
            file.sync_all()?;
            Ok(n)
        })();
        let result = written.and_then(|n| fs::rename(&tmp, &target).map(|_| n));
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result.map_err(not_found(path))
    }

    fn rename(&self, from: &RemotePath, to: &RemotePath) -> Result<()> {
        fs::rename(self.local_path(from), self.local_path(to)).map_err(not_found(from))
    }

    fn remove(&self, path: &RemotePath) -> Result<()> {
        match fs::remove_file(self.local_path(path)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    fn make_dirs(&self, dir: &RemotePath) -> Result<()> {
        Ok(fs::create_dir_all(self.local_path(dir))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::is_temporary;

    fn scratch(name: &str) -> LocalFolder {
        let dir = std::env::temp_dir().join(format!("sync-remote-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        LocalFolder::new(dir)
    }

    fn p(s: &str) -> RemotePath {
        RemotePath::new(s).unwrap()
    }

    #[test]
    fn writes_read_back_and_replace_in_one_step() {
        let share = scratch("rw");
        share.make_dirs(&p("a/b")).unwrap();
        share.write(&p("a/b/f.txt"), b"one").unwrap();
        share.write(&p("a/b/f.txt"), b"two!").unwrap();
        assert_eq!(share.read(&p("a/b/f.txt")).unwrap(), b"two!");
        assert_eq!(share.read_range(&p("a/b/f.txt"), 1, 2).unwrap(), b"wo");
        assert_eq!(share.read_range(&p("a/b/f.txt"), 3, 10).unwrap(), b"!");
        let listed = share.list(&p("a/b")).unwrap();
        assert_eq!(listed.len(), 1, "no temporary file is left: {listed:?}");
        assert_eq!(listed[0].name, "f.txt");
        assert_eq!(listed[0].size, 4);
        assert!(!listed[0].is_dir);
        assert!(listed[0].mtime_ms > 0);
        assert!(share.list(&p("a")).unwrap()[0].is_dir);
        let _ = fs::remove_dir_all(share.root());
    }

    #[test]
    fn missing_things_say_so() {
        let share = scratch("missing");
        assert!(matches!(share.list(&p("nope")), Err(Error::NotFound(_))));
        assert!(matches!(share.read(&p("nope.txt")), Err(Error::NotFound(_))));
        assert!(share.stat(&p("nope.txt")).unwrap().is_none());
        share.remove(&p("nope.txt")).unwrap();
        assert!(matches!(share.write(&p("no/folder.txt"), b"x"), Err(Error::NotFound(_))));
        assert!(share.list(&RemotePath::root()).unwrap().iter().all(|e| !is_temporary(&e.name)));
        let _ = fs::remove_dir_all(share.root());
    }

    #[test]
    fn renames_replace_and_remove_takes_one_file() {
        let share = scratch("mv");
        share.write(&p("x"), b"x").unwrap();
        share.write(&p("y"), b"y").unwrap();
        share.rename(&p("x"), &p("y")).unwrap();
        assert_eq!(share.read(&p("y")).unwrap(), b"x");
        assert!(share.stat(&p("x")).unwrap().is_none());
        share.make_dirs(&p("d")).unwrap();
        assert!(share.remove(&p("d")).is_err(), "a folder is never removed");
        share.remove(&p("y")).unwrap();
        assert!(share.stat(&p("y")).unwrap().is_none());
        let _ = fs::remove_dir_all(share.root());
    }
}
