//! Store and forward through a real WebDAV share (Droidtop/tracker#800 item 3): one device posts, the other
//! collects and removes. CI's WebDAV server is named as in sync-remote's tests/webdav_live.rs; without it the
//! test says so and passes.

use std::env;
use std::time::Duration;

use droidtop_agent_core::keys::DeviceKey;
use droidtop_agent_core::mailbox::{self, Envelope};
use sync_remote::{Remote, WebDav, WebDavConfig};

#[test]
fn letters_travel_through_a_webdav_share() {
    let Some(url) = env::var("SYNC_REMOTE_WEBDAV_URL").ok().filter(|u| !u.is_empty()) else {
        assert!(env::var("SYNC_REMOTE_WEBDAV_REQUIRED").is_err(), "SYNC_REMOTE_WEBDAV_REQUIRED is set but no server is named");
        eprintln!("no WebDAV server named in SYNC_REMOTE_WEBDAV_URL; skipped");
        return;
    };
    let config = WebDavConfig::new(&url)
        .sign_in(env::var("SYNC_REMOTE_WEBDAV_USER").unwrap_or_default(), env::var("SYNC_REMOTE_WEBDAV_PASSWORD").unwrap_or_default())
        .allow_plain_http(url.starts_with("http://127.0.0.1"))
        .timeout(Duration::from_secs(10));
    let share = WebDav::new(config).unwrap();
    let handheld = DeviceKey::generate();
    let pc = DeviceKey::generate();

    let payload = mailbox::pack(&Envelope::Library { changes: Vec::new() }, &[]).unwrap();
    mailbox::post_to(&share, &handheld, &pc.peer_id(), &payload).unwrap();
    mailbox::post_to(&share, &handheld, &pc.peer_id(), &payload).unwrap();

    let (letters, failed) = mailbox::collect_from(&share, &pc, |p| *p == handheld.peer_id()).unwrap();
    assert!(failed.is_empty(), "{:?}", failed);
    assert_eq!(letters.len(), 2);
    for letter in &letters {
        assert_eq!(letter.from, handheld.peer_id());
        assert!(matches!(mailbox::unpack(&letter.payload).unwrap().0, Envelope::Library { .. }));
        share.remove(&letter.path).unwrap();
    }
    let (left, _) = mailbox::collect_from(&share, &pc, |_| true).unwrap();
    assert!(left.is_empty());
    // Nobody else can open them, and a device that has none gets none.
    let (none, _) = mailbox::collect_from(&share, &handheld, |_| true).unwrap();
    assert!(none.is_empty());
}
