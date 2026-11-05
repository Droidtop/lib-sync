//! The WebDAV backend against a real WebDAV server. CI starts one (wsgidav, `.github/workflows/ci.yml`) and
//! names it in `SYNC_REMOTE_WEBDAV_URL`, with `SYNC_REMOTE_WEBDAV_USER` and `SYNC_REMOTE_WEBDAV_PASSWORD`.
//! Without them the test says so and passes, so `cargo test` works anywhere; CI fails if the server is not
//! named (`SYNC_REMOTE_WEBDAV_REQUIRED`).

use std::env;
use std::time::Duration;

use sync_remote::{is_temporary, Error, Remote, RemotePath, WebDav, WebDavConfig};

fn server() -> Option<(String, String, String)> {
    let url = env::var("SYNC_REMOTE_WEBDAV_URL").ok().filter(|u| !u.is_empty());
    if url.is_none() && env::var("SYNC_REMOTE_WEBDAV_REQUIRED").is_ok() {
        panic!("SYNC_REMOTE_WEBDAV_REQUIRED is set but SYNC_REMOTE_WEBDAV_URL is not");
    }
    let url = url?;
    Some((url, env::var("SYNC_REMOTE_WEBDAV_USER").unwrap_or_default(), env::var("SYNC_REMOTE_WEBDAV_PASSWORD").unwrap_or_default()))
}

fn p(s: &str) -> RemotePath {
    RemotePath::new(s).unwrap()
}

fn config(url: &str) -> WebDavConfig {
    WebDavConfig::new(url).allow_plain_http(url.starts_with("http://127.0.0.1")).timeout(Duration::from_secs(10))
}

#[test]
fn a_real_webdav_server() {
    let Some((url, user, password)) = server() else {
        eprintln!("no WebDAV server named in SYNC_REMOTE_WEBDAV_URL; skipped");
        return;
    };
    let dav = WebDav::new(config(&url).sign_in(&user, &password)).unwrap();
    let run = format!("run-{}", std::process::id());
    let dir = p(&format!("{run}/saves/Game One"));

    // Folders are made level by level, and making them again is fine.
    dav.make_dirs(&dir).unwrap();
    dav.make_dirs(&dir).unwrap();

    // A write lands in one step and replaces what was there; nothing temporary stays.
    let file = dir.join("slot 1.sav").unwrap();
    dav.write(&file, b"first").unwrap();
    dav.write(&file, b"second save").unwrap();
    assert_eq!(dav.read(&file).unwrap(), b"second save");
    assert_eq!(dav.read_range(&file, 7, 4).unwrap(), b"save");
    assert_eq!(dav.read_range(&file, 7, 100).unwrap(), b"save");
    let listed = dav.list(&dir).unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].name, "slot 1.sav");
    assert_eq!(listed[0].size, 11);
    assert!(!listed[0].is_dir);
    assert!(listed.iter().all(|e| !is_temporary(&e.name)));
    let parent = dav.list(&p(&format!("{run}/saves"))).unwrap();
    assert_eq!(parent.len(), 1);
    assert!(parent[0].is_dir);
    assert_eq!(parent[0].name, "Game One");

    // A large file goes up streamed, not buffered.
    let big: Vec<u8> = (0..(9u32 << 20)).map(|i| (i % 251) as u8).collect();
    let big_file = dir.join("big.bin").unwrap();
    dav.write(&big_file, &big).unwrap();
    assert_eq!(dav.stat(&big_file).unwrap().unwrap().size, big.len() as u64);
    assert_eq!(dav.read_range(&big_file, 5_000_000, 3).unwrap(), big[5_000_000..5_000_003].to_vec());
    dav.remove(&big_file).unwrap();

    // Rename replaces; stat and remove say what is there.
    let other = dir.join("slot 2.sav").unwrap();
    dav.write(&other, b"two").unwrap();
    dav.rename(&other, &file).unwrap();
    assert_eq!(dav.read(&file).unwrap(), b"two");
    assert!(dav.stat(&other).unwrap().is_none());
    assert!(dav.stat(&dir).unwrap().unwrap().is_dir);

    // Missing things and folders.
    assert!(matches!(dav.read(&dir.join("none").unwrap()), Err(Error::NotFound(_))));
    assert!(matches!(dav.list(&p(&format!("{run}/none"))), Err(Error::NotFound(_))));
    assert!(matches!(dav.write(&p(&format!("{run}/no/such/x")), b"x"), Err(Error::NotFound(_))));
    dav.remove(&dir.join("none").unwrap()).unwrap();
    assert!(matches!(dav.remove(&dir), Err(Error::Refused(_))), "a folder is never removed");
    assert!(dav.stat(&file).unwrap().is_some(), "refusing the folder left its files");
    dav.remove(&file).unwrap();
    assert!(dav.list(&dir).unwrap().is_empty());

    // A wrong password is a refused sign-in, not something else.
    if !user.is_empty() {
        let wrong = WebDav::new(config(&url).sign_in(&user, "not the password")).unwrap();
        assert!(matches!(wrong.list(&RemotePath::root()), Err(Error::SignInRefused)));
    }
}

#[test]
fn a_share_that_does_not_answer_is_unreachable() {
    // Port 9 on this computer: nothing listens there.
    let dav = WebDav::new(config("http://127.0.0.1:9/dav")).unwrap();
    assert!(matches!(dav.list(&RemotePath::root()), Err(Error::Unreachable(_))));
}
