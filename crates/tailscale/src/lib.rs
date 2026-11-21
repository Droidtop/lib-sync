//! The local Tailscale node, through its LocalAPI (Droidtop/tracker#645). The owner, 2026-11-05: "We control
//! tailscale like any other VPN" and "Look at the tailscale code, ktailctl ... We're building a local tailscale
//! control plugin". So this is a client of the node the person already runs, never a node of its own:
//!
//! - **Where:** tailscaled's LocalAPI socket (`/var/run/tailscale/tailscaled.sock`, also `/run/tailscale/...`) on
//!   Linux and the BSDs, its named pipe (`\\.\pipe\ProtectedPrefix\Administrators\Tailscale\tailscaled`) on Windows,
//!   as tailscale's `safesocket` names them. macOS's App Store build uses a different transport and is not reached.
//! - **What:** `GET /localapi/v0/status`, the call behind `tailscale status --json`, ktailctl's peer list and the
//!   official apps' device list: `BackendState`, `Self` and `Peer` with `HostName`, `DNSName`, `OS`,
//!   `TailscaleIPs` and `Online`. Reading status needs no operator rights.
//! - **When absent:** no socket, a stopped daemon or a logged-out node is [`Tailnet::Absent`] or not running,
//!   never an error to show. Tailscale is used when present, never required (#373).

use std::io::{self, BufRead, BufReader, Read, Write};
use std::time::Duration;

use serde_json::Value;

/// The `Host` tailscaled's LocalAPI expects (tailscale `apitype.LocalAPIHost`).
pub const LOCALAPI_HOST: &str = "local-tailscaled.sock";
const STATUS_PATH: &str = "/localapi/v0/status";
/// A status reply is a few KiB per peer; a tailnet of thousands stays well under this.
const MAX_REPLY: usize = 16 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("tailscaled did not answer: {0}")]
    Io(#[from] io::Error),
    #[error("tailscaled answered HTTP {0}")]
    Status(u16),
    #[error("tailscaled's answer was not understood: {0}")]
    Reply(String),
}

/// One device on the tailnet, as `status` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub host_name: String,
    /// The MagicDNS name, without its final dot (`pc.tail1234.ts.net`).
    pub dns_name: String,
    pub os: String,
    /// Its Tailscale addresses, IPv4 (100.64.0.0/10) first, then IPv6 (fd7a:115c:a1e0::/48).
    pub addresses: Vec<String>,
    pub online: bool,
}

/// The tailnet as this computer's node sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tailnet {
    /// No tailscaled here (no socket, or nothing listening on it).
    Absent,
    /// tailscaled runs. `state` is its `BackendState` (`Running`, `Stopped`, `NeedsLogin`, `Starting`...).
    Present { state: String, me: Option<Peer>, peers: Vec<Peer> },
}

impl Tailnet {
    pub fn running(&self) -> bool {
        matches!(self, Tailnet::Present { state, .. } if state == "Running")
    }

    /// The addresses of every online peer, for a program to reach them at (lib-kdeconnect's `addresses`).
    pub fn online_addresses(&self) -> Vec<String> {
        match self {
            Tailnet::Present { peers, .. } if self.running() => {
                peers.iter().filter(|p| p.online).flat_map(|p| p.addresses.iter().cloned()).collect()
            }
            _ => Vec::new(),
        }
    }
}

/// Reads a `status` reply's JSON (tailscale `ipnstate.Status`).
pub fn parse_status(json: &[u8]) -> Result<Tailnet, Error> {
    let v: Value = serde_json::from_slice(json).map_err(|e| Error::Reply(e.to_string()))?;
    let state = v.get("BackendState").and_then(Value::as_str).unwrap_or("").to_owned();
    let me = v.get("Self").filter(|s| s.is_object()).map(peer_of);
    let mut peers: Vec<Peer> = v.get("Peer").and_then(Value::as_object).map(|m| m.values().map(peer_of).collect()).unwrap_or_default();
    peers.sort_by(|a, b| b.online.cmp(&a.online).then_with(|| a.host_name.to_lowercase().cmp(&b.host_name.to_lowercase())));
    Ok(Tailnet::Present { state, me, peers })
}

fn peer_of(v: &Value) -> Peer {
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
    let mut addresses: Vec<String> = v
        .get("TailscaleIPs")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
        .unwrap_or_default();
    addresses.sort_by_key(|a| a.contains(':'));
    Peer {
        host_name: s("HostName"),
        dns_name: s("DNSName").trim_end_matches('.').to_owned(),
        os: s("OS"),
        addresses,
        online: v.get("Online").and_then(Value::as_bool).unwrap_or(false),
    }
}

/// Asks the local node for its status. [`Tailnet::Absent`] when there is none.
pub fn status() -> Result<Tailnet, Error> {
    match transport::connect() {
        Ok(Some(mut stream)) => {
            let body = get(&mut stream, STATUS_PATH)?;
            parse_status(&body)
        }
        Ok(None) => Ok(Tailnet::Absent),
        Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused) => Ok(Tailnet::Absent),
        Err(e) => Err(e.into()),
    }
}

/// One HTTP/1.1 GET over an open LocalAPI stream, the body read whole (plain or chunked).
pub fn get<S: Read + Write>(stream: &mut S, path: &str) -> Result<Vec<u8>, Error> {
    write!(stream, "GET {path} HTTP/1.1\r\nHost: {LOCALAPI_HOST}\r\nConnection: close\r\n\r\n")?;
    stream.flush()?;
    let mut r = BufReader::new(stream);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let code: u16 =
        line.split_whitespace().nth(1).and_then(|c| c.parse().ok()).ok_or_else(|| Error::Reply(format!("status line {line:?}")))?;
    let (mut length, mut chunked) = (None, false);
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        let (k, val) = line.split_once(':').unwrap_or((&line, ""));
        let (k, val) = (k.trim().to_ascii_lowercase(), val.trim());
        if k == "content-length" {
            length = val.parse::<usize>().ok();
        } else if k == "transfer-encoding" && val.eq_ignore_ascii_case("chunked") {
            chunked = true;
        }
    }
    let body = if chunked {
        let mut body = Vec::new();
        loop {
            line.clear();
            r.read_line(&mut line)?;
            let size =
                usize::from_str_radix(line.trim().split(';').next().unwrap_or(""), 16).map_err(|_| Error::Reply("chunk size".into()))?;
            if size == 0 {
                break;
            }
            if body.len() + size > MAX_REPLY {
                return Err(Error::Reply("reply too long".into()));
            }
            let start = body.len();
            body.resize(start + size, 0);
            r.read_exact(&mut body[start..])?;
            line.clear();
            r.read_line(&mut line)?;
        }
        body
    } else if let Some(n) = length {
        if n > MAX_REPLY {
            return Err(Error::Reply("reply too long".into()));
        }
        let mut body = vec![0; n];
        r.read_exact(&mut body)?;
        body
    } else {
        let mut body = Vec::new();
        r.take(MAX_REPLY as u64 + 1).read_to_end(&mut body)?;
        body
    };
    if code != 200 {
        return Err(Error::Status(code));
    }
    Ok(body)
}

#[cfg(unix)]
mod transport {
    use super::TIMEOUT;
    use std::io;
    use std::os::unix::net::UnixStream;

    /// tailscale `safesocket`'s default paths, and `TAILSCALE_SOCKET` (tests, and a tailscaled started with
    /// `--socket`).
    fn paths() -> Vec<String> {
        let mut v: Vec<String> = std::env::var("TAILSCALE_SOCKET").into_iter().collect();
        v.extend(["/var/run/tailscale/tailscaled.sock", "/run/tailscale/tailscaled.sock"].map(String::from));
        v
    }

    pub fn connect() -> io::Result<Option<UnixStream>> {
        for p in paths() {
            if std::path::Path::new(&p).exists() {
                let s = UnixStream::connect(&p)?;
                s.set_read_timeout(Some(TIMEOUT))?;
                s.set_write_timeout(Some(TIMEOUT))?;
                return Ok(Some(s));
            }
        }
        Ok(None)
    }
}

#[cfg(windows)]
mod transport {
    use std::fs::{File, OpenOptions};
    use std::io;

    /// tailscale `safesocket`'s Windows pipe.
    const PIPE: &str = r"\\.\pipe\ProtectedPrefix\Administrators\Tailscale\tailscaled";

    pub fn connect() -> io::Result<Option<File>> {
        match OpenOptions::new().read(true).write(true).open(PIPE) {
            Ok(f) => Ok(Some(f)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod transport {
    pub fn connect() -> std::io::Result<Option<std::fs::File>> {
        Ok(None)
    }
}
