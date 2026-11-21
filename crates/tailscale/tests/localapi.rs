//! The LocalAPI client against a fake tailscaled on a Unix socket, answering `status` as tailscaled does (chunked).

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;

use net_tailscale::{parse_status, status, Tailnet};

// A trimmed `tailscale status --json` (ipnstate.Status).
const STATUS: &str = r#"{"Version":"1.90.0","BackendState":"Running",
 "Self":{"HostName":"handheld","DNSName":"handheld.tail1234.ts.net.","OS":"android","TailscaleIPs":["fd7a:115c:a1e0::1","100.101.1.1"],"Online":true},
 "Peer":{"nodekey:aa":{"HostName":"living-room-pc","DNSName":"living-room-pc.tail1234.ts.net.","OS":"windows","TailscaleIPs":["100.101.1.2","fd7a:115c:a1e0::2"],"Online":true},
         "nodekey:bb":{"HostName":"laptop","DNSName":"laptop.tail1234.ts.net.","OS":"linux","TailscaleIPs":["100.101.1.3"],"Online":false}}}"#;

#[test]
fn reads_status() {
    let t = parse_status(STATUS.as_bytes()).unwrap();
    assert!(t.running());
    let Tailnet::Present { me, peers, .. } = &t else { panic!() };
    assert_eq!(me.as_ref().unwrap().addresses, vec!["100.101.1.1", "fd7a:115c:a1e0::1"]);
    assert_eq!(peers[0].host_name, "living-room-pc");
    assert_eq!(peers[0].dns_name, "living-room-pc.tail1234.ts.net");
    assert!(!peers[1].online);
    assert_eq!(t.online_addresses(), vec!["100.101.1.2", "fd7a:115c:a1e0::2"]);
    let stopped = parse_status(br#"{"BackendState":"Stopped","Peer":{}}"#).unwrap();
    assert!(!stopped.running());
    assert!(stopped.online_addresses().is_empty());
    assert!(parse_status(b"nope").is_err());
}

#[test]
fn asks_a_tailscaled_socket_and_says_absent_without_one() {
    let dir = std::env::temp_dir().join(format!("nts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("tailscaled.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let server = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut first = String::new();
        r.read_line(&mut first).unwrap();
        let mut host = String::new();
        loop {
            let mut l = String::new();
            r.read_line(&mut l).unwrap();
            if l.to_ascii_lowercase().starts_with("host:") {
                host = l.trim().to_owned();
            }
            if l == "\r\n" {
                break;
            }
        }
        // Chunked, in two chunks, as Go's net/http writes a streamed JSON body.
        let (a, b) = STATUS.split_at(100);
        write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{a}\r\n{:x}\r\n{b}\r\n0\r\n\r\n", a.len(), b.len()).unwrap();
        (first, host)
    });
    std::env::set_var("TAILSCALE_SOCKET", &sock);
    let t = status().unwrap();
    let (first, host) = server.join().unwrap();
    assert_eq!(first.trim(), "GET /localapi/v0/status HTTP/1.1");
    assert_eq!(host, "Host: local-tailscaled.sock");
    assert_eq!(t.online_addresses().len(), 2);

    std::fs::remove_file(&sock).unwrap();
    std::env::set_var("TAILSCALE_SOCKET", dir.join("missing.sock"));
    // On this machine tailscaled's own socket may exist; only a machine without one reads Absent.
    if !std::path::Path::new("/var/run/tailscale/tailscaled.sock").exists()
        && !std::path::Path::new("/run/tailscale/tailscaled.sock").exists()
    {
        assert_eq!(status().unwrap(), Tailnet::Absent);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
