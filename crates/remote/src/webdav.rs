//! WebDAV (RFC 4918): Nextcloud, ownCloud, most NAS, Koofr, pCloud, Box and any WebDAV server
//! (Droidtop/tracker#573 design D2, item 1; the store-and-forward backend of Droidtop/tracker#800).
//!
//! The requests used: PROPFIND (depth 0 and 1), GET (with Range), PUT, MOVE, DELETE and MKCOL. The
//! sign-in is HTTP Basic, sent only over https unless the person allowed plain http for this share.
//! Redirects are not followed, so the sign-in never goes to another address than the one the person
//! typed; a share that moved says where to.

use std::io::{self, Read, Write};
use std::time::{Duration, UNIX_EPOCH};

use base64::Engine as _;
use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, CONTROLS};

use crate::{temporary_name, Entry, Error, Remote, RemotePath, Result};

/// The characters escaped in one path segment (RFC 3986 `pchar` keeps the rest).
const SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'/')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

/// What a WebDAV share is: its address and the person's own sign-in.
#[derive(Clone)]
#[non_exhaustive]
pub struct WebDavConfig {
    /// The folder the share starts at, for example `https://cloud.example.com/remote.php/dav/files/me/`.
    /// Never with a user name or password in it.
    pub url: String,
    pub user: Option<String>,
    pub password: Option<String>,
    /// Plain http is refused unless this is set: the person's choice, for a share on their own network.
    pub allow_plain_http: bool,
    /// How long to wait for the share to answer before it counts as unreachable.
    pub timeout: Duration,
}

impl WebDavConfig {
    pub fn new(url: impl Into<String>) -> WebDavConfig {
        WebDavConfig { url: url.into(), user: None, password: None, allow_plain_http: false, timeout: Duration::from_secs(30) }
    }

    pub fn sign_in(mut self, user: impl Into<String>, password: impl Into<String>) -> WebDavConfig {
        self.user = Some(user.into());
        self.password = Some(password.into());
        self
    }

    pub fn allow_plain_http(mut self, allow: bool) -> WebDavConfig {
        self.allow_plain_http = allow;
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> WebDavConfig {
        self.timeout = timeout;
        self
    }
}

impl std::fmt::Debug for WebDavConfig {
    // The password is never printed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebDavConfig")
            .field("url", &self.url)
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "…"))
            .field("allow_plain_http", &self.allow_plain_http)
            .finish()
    }
}

/// A WebDAV share.
pub struct WebDav {
    agent: ureq::Agent,
    /// The base address without its trailing `/`.
    base: String,
    /// The base address's path, decoded, without slashes at either end: how the share names its root in a listing.
    base_path: String,
    authorization: Option<String>,
}

/// Bodies up to this size are sent with their length; larger ones are streamed in chunks.
const BUFFERED_UPLOAD: usize = 8 << 20;

const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/><d:getcontentlength/><d:getlastmodified/></d:prop></d:propfind>"#;

impl WebDav {
    pub fn new(config: WebDavConfig) -> Result<WebDav> {
        let url = config.url.trim();
        let (scheme, rest) = url.split_once("://").ok_or_else(|| Error::BadPath(url.to_string()))?;
        match scheme.to_ascii_lowercase().as_str() {
            "https" => {}
            "http" if config.allow_plain_http => {}
            "http" => return Err(Error::NotEncrypted),
            _ => return Err(Error::BadPath(url.to_string())),
        }
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        // A sign-in in the address would end up in logs and history: it goes in the sign-in fields instead.
        if authority.is_empty() || authority.contains('@') || path.contains('?') || path.contains('#') {
            return Err(Error::BadPath(url.to_string()));
        }
        let base = url.trim_end_matches('/').to_string();
        let base_path = percent_decode_str(path).decode_utf8_lossy().trim_matches('/').to_string();
        let authorization = config.user.as_ref().map(|user| {
            let pair = format!("{user}:{}", config.password.as_deref().unwrap_or(""));
            format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(pair))
        });
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(config.timeout.min(Duration::from_secs(15)))
            .timeout_read(config.timeout)
            .timeout_write(config.timeout)
            .redirects(0)
            .user_agent(concat!("droidtop-sync-remote/", env!("CARGO_PKG_VERSION")))
            .build();
        Ok(WebDav { agent, base, base_path, authorization })
    }

    fn url(&self, path: &RemotePath, folder: bool) -> String {
        let mut url = self.base.clone();
        for s in path.segments() {
            url.push('/');
            url.extend(utf8_percent_encode(s, SEGMENT));
        }
        if folder {
            url.push('/');
        }
        url
    }

    fn request(&self, method: &str, url: &str) -> ureq::Request {
        let r = self.agent.request(method, url);
        match &self.authorization {
            Some(a) => r.set("Authorization", a),
            None => r,
        }
    }

    fn propfind(&self, path: &RemotePath, depth: &str) -> Result<Vec<(String, Entry)>> {
        let url = self.url(path, true);
        let response = self
            .request("PROPFIND", &url)
            .set("Depth", depth)
            .set("Content-Type", "application/xml; charset=utf-8")
            .send_string(PROPFIND_BODY)
            .map_err(|e| failure(e, path))?;
        if response.status() != 207 {
            return Err(Error::Protocol(format!("PROPFIND answered {}", response.status())));
        }
        let mut body = String::new();
        response.into_reader().take(64 << 20).read_to_string(&mut body)?;
        parse_multistatus(&body)
    }

    /// The decoded path of [`path`] as the share names it in a listing, without slashes at either end.
    fn listed_path(&self, path: &RemotePath) -> String {
        match (self.base_path.is_empty(), path.is_root()) {
            (_, true) => self.base_path.clone(),
            (true, false) => path.as_str().to_string(),
            (false, false) => format!("{}/{}", self.base_path, path.as_str()),
        }
    }
}

/// An error from ureq as one of [`Error`], for an operation on [`path`].
fn failure(e: ureq::Error, path: &RemotePath) -> Error {
    match e {
        ureq::Error::Status(code, response) => match code {
            401 => Error::SignInRefused,
            404 | 410 => Error::NotFound(path.to_string()),
            // A PUT, MKCOL or MOVE into a folder that is not there.
            409 => Error::NotFound(path.parent().map(|p| p.to_string()).unwrap_or_else(|| path.to_string())),
            301 | 302 | 303 | 307 | 308 => {
                Error::Refused(format!("the share moved to {}; use that address", response.header("Location").unwrap_or("another address")))
            }
            403 => Error::Refused("not allowed".into()),
            423 => Error::Refused("the file is locked".into()),
            507 => Error::Refused("the share is full".into()),
            other => Error::Refused(format!("it answered {other}")),
        },
        ureq::Error::Transport(t) => Error::Unreachable(t.to_string()),
    }
}

/// The href (decoded, without slashes at either end) and entry of each response in a PROPFIND answer.
fn parse_multistatus(body: &str) -> Result<Vec<(String, Entry)>> {
    let doc = roxmltree::Document::parse(body).map_err(|e| Error::Protocol(e.to_string()))?;
    let dav = |n: &roxmltree::Node, name: &str| n.tag_name().name() == name && n.tag_name().namespace() == Some("DAV:");
    let mut out = Vec::new();
    for response in doc.descendants().filter(|n| dav(n, "response")) {
        let Some(href) = response.children().find(|n| dav(n, "href")).and_then(|n| n.text()) else { continue };
        // An href is a path, or a whole address: keep the path.
        let href = href.trim();
        let path = match href.split_once("://") {
            Some((_, rest)) => rest.split_once('/').map(|(_, p)| p).unwrap_or(""),
            None => href,
        };
        let path = percent_decode_str(path).decode_utf8_lossy().trim_matches('/').to_string();
        // The properties from the propstat whose status is 200.
        let ok = response
            .children()
            .filter(|n| dav(n, "propstat"))
            .find(|ps| ps.children().find(|n| dav(n, "status")).and_then(|n| n.text()).is_none_or(|s| s.contains(" 200")));
        let Some(prop) = ok.and_then(|ps| ps.children().find(|n| dav(n, "prop"))) else { continue };
        let is_dir = prop.children().find(|n| dav(n, "resourcetype")).is_some_and(|rt| rt.children().any(|n| dav(&n, "collection")));
        let text = |name: &str| prop.children().find(|n| dav(n, name)).and_then(|n| n.text()).map(str::trim);
        let size = if is_dir { 0 } else { text("getcontentlength").and_then(|s| s.parse().ok()).unwrap_or(0) };
        let mtime_ms = text("getlastmodified")
            .and_then(|s| httpdate::parse_http_date(s).ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let name = path.rsplit('/').next().unwrap_or("").to_string();
        out.push((path, Entry::new(name, is_dir, size, mtime_ms)));
    }
    Ok(out)
}

impl Remote for WebDav {
    fn list(&self, dir: &RemotePath) -> Result<Vec<Entry>> {
        let own = self.listed_path(dir);
        Ok(self
            .propfind(dir, "1")?
            .into_iter()
            .filter(|(href, entry)| *href != own && RemotePath::root().join(&entry.name).is_ok())
            .map(|(_, entry)| entry)
            .collect())
    }

    fn stat(&self, path: &RemotePath) -> Result<Option<Entry>> {
        match self.propfind(path, "0") {
            Ok(found) => Ok(found.into_iter().next().map(|(_, mut e)| {
                e.name = path.name().to_string();
                e
            })),
            Err(Error::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn read_into(&self, path: &RemotePath, sink: &mut dyn Write) -> Result<u64> {
        let response = self.request("GET", &self.url(path, false)).call().map_err(|e| failure(e, path))?;
        Ok(io::copy(&mut response.into_reader(), sink)?)
    }

    fn read_range(&self, path: &RemotePath, offset: u64, len: u64) -> Result<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let range = format!("bytes={offset}-{}", offset.saturating_add(len - 1));
        let response = match self.request("GET", &self.url(path, false)).set("Range", &range).call() {
            Ok(r) => r,
            Err(ureq::Error::Status(416, _)) => return Ok(Vec::new()),
            Err(e) => return Err(failure(e, path)),
        };
        let whole = response.status() == 200;
        let mut reader = response.into_reader();
        if whole {
            // A share that ignores Range sends the whole file: skip to the offset.
            io::copy(&mut (&mut reader).take(offset), &mut io::sink())?;
        }
        let mut out = Vec::new();
        reader.take(len).read_to_end(&mut out)?;
        Ok(out)
    }

    fn write_from(&self, path: &RemotePath, source: &mut dyn Read) -> Result<u64> {
        let tmp = temporary_name(path)?;
        let tmp_url = self.url(&tmp, false);
        let mut head = Vec::new();
        source.take(BUFFERED_UPLOAD as u64 + 1).read_to_end(&mut head)?;
        let put = self.request("PUT", &tmp_url).set("Content-Type", "application/octet-stream");
        let (sent, count) = if head.len() <= BUFFERED_UPLOAD {
            let count = head.len() as u64;
            (put.send_bytes(&head), count)
        } else {
            let mut counted = Counted { inner: io::Cursor::new(head).chain(source), count: 0 };
            let sent = put.send(&mut counted);
            (sent, counted.count)
        };
        let result = sent.map_err(|e| failure(e, &tmp)).and_then(|_| self.rename(&tmp, path));
        if result.is_err() {
            let _ = self.request("DELETE", &tmp_url).call();
        }
        result.map(|_| count)
    }

    fn rename(&self, from: &RemotePath, to: &RemotePath) -> Result<()> {
        self.request("MOVE", &self.url(from, false))
            .set("Destination", &self.url(to, false))
            .set("Overwrite", "T")
            .call()
            .map(|_| ())
            .map_err(|e| failure(e, from))
    }

    fn remove(&self, path: &RemotePath) -> Result<()> {
        // DELETE on a folder takes everything in it: refuse folders before asking.
        match self.stat(path)? {
            None => Ok(()),
            Some(e) if e.is_dir => Err(Error::Refused(format!("{path} is a folder"))),
            Some(_) => match self.request("DELETE", &self.url(path, false)).call() {
                Ok(_) | Err(ureq::Error::Status(404, _)) => Ok(()),
                Err(e) => Err(failure(e, path)),
            },
        }
    }

    fn make_dirs(&self, dir: &RemotePath) -> Result<()> {
        let mut at = RemotePath::root();
        for segment in dir.segments().map(str::to_string).collect::<Vec<_>>() {
            at = at.join(&segment)?;
            match self.request("MKCOL", &self.url(&at, true)).call() {
                // 405: it is already there.
                Ok(_) | Err(ureq::Error::Status(405, _)) => {}
                Err(e) => return Err(failure(e, &at)),
            }
        }
        Ok(())
    }
}

struct Counted<R> {
    inner: R,
    count: u64,
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count += n as u64;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The shape Nextcloud and wsgidav answer a depth-1 PROPFIND with (RFC 4918 section 9.1).
    const LISTING: &str = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:s="http://sabredav.org/ns">
 <d:response><d:href>/remote.php/dav/files/me/Saves/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype><d:getlastmodified>Tue, 03 Nov 2026 10:00:00 GMT</d:getlastmodified></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
  <d:propstat><d:prop><d:getcontentlength/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat></d:response>
 <d:response><d:href>/remote.php/dav/files/me/Saves/slot%201.sav</d:href>
  <d:propstat><d:prop><d:resourcetype/><d:getcontentlength>1234</d:getcontentlength><d:getlastmodified>Wed, 04 Nov 2026 08:30:00 GMT</d:getlastmodified></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
 <d:response><d:href>https://cloud.example.com/remote.php/dav/files/me/Saves/Old%20games/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
</d:multistatus>"#;

    #[test]
    fn a_listing_reads_files_and_folders() {
        let parsed = parse_multistatus(LISTING).unwrap();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].0, "remote.php/dav/files/me/Saves");
        assert!(parsed[0].1.is_dir);
        let (href, file) = &parsed[1];
        assert_eq!(href, "remote.php/dav/files/me/Saves/slot 1.sav");
        assert_eq!(file.name, "slot 1.sav");
        assert_eq!(file.size, 1234);
        assert!(!file.is_dir);
        assert_eq!(file.mtime_ms, 1_793_781_000_000);
        assert_eq!(parsed[2].1.name, "Old games");
        assert!(parsed[2].1.is_dir);
    }

    #[test]
    fn the_share_names_itself_in_a_listing() {
        let dav = WebDav::new(WebDavConfig::new("https://cloud.example.com/remote.php/dav/files/me/")).unwrap();
        let saves = RemotePath::new("Saves").unwrap();
        assert_eq!(dav.listed_path(&saves), "remote.php/dav/files/me/Saves");
        assert_eq!(dav.listed_path(&RemotePath::root()), "remote.php/dav/files/me");
        assert_eq!(
            dav.url(&RemotePath::new("Saves/slot 1#.sav").unwrap(), false),
            "https://cloud.example.com/remote.php/dav/files/me/Saves/slot%201%23.sav"
        );
        assert_eq!(dav.url(&saves, true), "https://cloud.example.com/remote.php/dav/files/me/Saves/");
        let top = WebDav::new(WebDavConfig::new("https://dav.example.com")).unwrap();
        assert_eq!(top.listed_path(&saves), "Saves");
    }

    #[test]
    fn addresses_that_would_leak_a_sign_in_are_refused() {
        assert!(matches!(WebDav::new(WebDavConfig::new("http://nas.local/dav")), Err(Error::NotEncrypted)));
        assert!(WebDav::new(WebDavConfig::new("http://nas.local/dav").allow_plain_http(true)).is_ok());
        assert!(WebDav::new(WebDavConfig::new("https://me:secret@cloud.example.com/dav")).is_err());
        assert!(WebDav::new(WebDavConfig::new("https://cloud.example.com/dav?token=x")).is_err());
        assert!(WebDav::new(WebDavConfig::new("ftp://cloud.example.com/")).is_err());
        assert!(WebDav::new(WebDavConfig::new("cloud.example.com")).is_err());
    }

    #[test]
    fn the_password_is_never_printed() {
        let config = WebDavConfig::new("https://cloud.example.com/").sign_in("me", "hunter2");
        assert!(!format!("{config:?}").contains("hunter2"));
    }
}
