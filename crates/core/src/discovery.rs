//! Finding each other on the LAN (docs/DESIGN.md sections 3 and 10): small
//! UDP broadcasts on the agent's port.
//!
//! - The handheld asks which of its paired computers are there; an agent
//!   answers only a device it has pinned, signs the answer with its key, and
//!   the handheld checks the signature, so a stranger cannot pose as the
//!   computer (and the TLS handshake would refuse it anyway).
//! - While the pairing screen is open, the handheld answers the computer's
//!   pairing query with its id, name and port. Nothing there is trusted:
//!   SPAKE2 is what authenticates the pairing.
//! - While a computer offers to pair by comparing codes ([`offer`]), it
//!   answers the query for computers that can pair (`DTCQ1`) with its id,
//!   name, pairing port and program (droidtop SPEC 7o, "Pairing with
//!   Pythia"). Nothing there is trusted either: the code both screens show
//!   is what authenticates (`crate::confirm`).

use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};

use crate::hex;
use crate::keys::{verify, DeviceKey, PeerId};

const QUERY: &[u8] = b"DTAQ1";
const ANSWER: &[u8] = b"DTAR1";
const PAIR_QUERY: &[u8] = b"DTPQ1";
const PAIR_ANSWER: &[u8] = b"DTPR1";
const COMPUTER_QUERY: &[u8] = b"DTCQ1";
const COMPUTER_ANSWER: &[u8] = b"DTCR1";

/// A computer that can pair by comparing codes, as it answers `DTCQ1`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub id: String,
    pub name: String,
    /// The TCP port its comparison listens on.
    pub port: u16,
    /// `pythia` or `droidtop-agent`.
    pub program: String,
}

struct PublishedOffer {
    offer: Offer,
    generation: Arc<()>,
}

static OFFER: Mutex<Option<PublishedOffer>> = Mutex::new(None);

/// Sets what this process answers `DTCQ1` with, or stops answering (None).
/// Every answering loop in the process reads it.
pub fn offer(o: Option<Offer>) {
    *OFFER.lock().unwrap_or_else(|e| e.into_inner()) = o.map(|offer| PublishedOffer { offer, generation: Arc::new(()) });
}

/// One publication's ownership. Dropping an old lease cannot clear a newer advertisement, even with identical fields.
pub struct OfferLease {
    generation: Arc<()>,
}

impl OfferLease {
    /// Withdraws this publication only, returning false if it has already been replaced or withdrawn.
    pub fn withdraw(&self) -> bool {
        let mut current = OFFER.lock().unwrap_or_else(|e| e.into_inner());
        if current.as_ref().is_some_and(|offer| Arc::ptr_eq(&offer.generation, &self.generation)) {
            *current = None;
            true
        } else {
            false
        }
    }
}

impl Drop for OfferLease {
    fn drop(&mut self) {
        self.withdraw();
    }
}

/// Publishes an offer with scoped cleanup. The existing [`offer`] setter remains compatible and replaces old leases.
pub fn offer_lease(offer: Offer) -> OfferLease {
    let generation = Arc::new(());
    *OFFER.lock().unwrap_or_else(|e| e.into_inner()) = Some(PublishedOffer { offer, generation: generation.clone() });
    OfferLease { generation }
}

fn current_offer() -> Option<Offer> {
    OFFER.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|current| current.offer.clone())
}

/// Answers a `DTCQ1` packet with the current offer; true when it was one.
fn answer_offer(sock: &UdpSocket, data: &[u8], from: SocketAddr) -> bool {
    if body(COMPUTER_QUERY, data).is_none() {
        return false;
    }
    if let Some(o) = current_offer() {
        let _ = sock.send_to(&packet(COMPUTER_ANSWER, &o), from);
    }
    true
}

#[derive(Serialize, Deserialize, Debug)]
struct Query {
    nonce: String,
    #[serde(default)]
    from: String,
}

/// A device's answer: who it is and the TCP port it listens on.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Announce {
    pub id: String,
    pub name: String,
    pub port: u16,
    #[serde(default)]
    pub sig: String,
}

/// A device found on the LAN, at its TCP address.
#[derive(Debug, Clone)]
pub struct Found {
    pub peer: PeerId,
    pub name: String,
    pub addr: SocketAddr,
}

fn nonce() -> String {
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(&bytes)
}

fn signed(nonce: &str, a: &Announce) -> Vec<u8> {
    format!("droidtop-agent announce v1\n{nonce}\n{}\n{}\n{}", a.id, a.name, a.port).into_bytes()
}

fn packet(magic: &[u8], body: &impl Serialize) -> Vec<u8> {
    let mut p = magic.to_vec();
    p.extend_from_slice(&serde_json::to_vec(body).unwrap_or_default());
    p
}

fn body<'a>(magic: &[u8], packet: &'a [u8]) -> Option<&'a [u8]> {
    packet.strip_prefix(magic)
}

fn broadcast_socket() -> io::Result<UdpSocket> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    sock.set_broadcast(true)?;
    Ok(sock)
}

/// Receives until [`deadline`], handing each packet to [`each`].
fn gather(sock: &UdpSocket, timeout: Duration, mut each: impl FnMut(&[u8], SocketAddr)) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 2048];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(());
        }
        sock.set_read_timeout(Some(left))?;
        match sock.recv_from(&mut buf) {
            Ok((n, from)) => each(&buf[..n], from),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => return Ok(()),
            Err(e) => return Err(e),
        }
    }
}

/// The handheld: which of [`wanted`] answer on the LAN within [`timeout`].
pub fn find_agents(me: &PeerId, wanted: &[PeerId], timeout: Duration) -> io::Result<Vec<Found>> {
    let sock = broadcast_socket()?;
    let nonce = nonce();
    sock.send_to(&packet(QUERY, &Query { nonce: nonce.clone(), from: me.to_hex() }), (Ipv4Addr::BROADCAST, crate::PORT))?;
    let mut found: Vec<Found> = Vec::new();
    gather(&sock, timeout, |data, from| {
        let Some(a) = body(ANSWER, data).and_then(|b| serde_json::from_slice::<Announce>(b).ok()) else { return };
        let Ok(peer) = PeerId::from_hex(&a.id) else { return };
        let sig = hex::decode(&a.sig).unwrap_or_default();
        if !wanted.contains(&peer) || !verify(&peer, &signed(&nonce, &a), &sig) || found.iter().any(|f| f.peer == peer) {
            return;
        }
        found.push(Found { peer, name: a.name, addr: SocketAddr::new(from.ip(), a.port) });
    })?;
    Ok(found)
}

/// The computer: answers the handheld's queries until [`stop`], on a socket
/// bound to the agent's port. Only devices [`trusted`] says are paired get
/// an answer.
pub fn answer_agent_queries(
    sock: &UdpSocket,
    key: &DeviceKey,
    name: &str,
    port: u16,
    trusted: impl Fn(&PeerId) -> bool,
    stop: &AtomicBool,
) -> io::Result<()> {
    sock.set_read_timeout(Some(Duration::from_millis(500)))?;
    let mut buf = [0u8; 2048];
    while !stop.load(Ordering::Relaxed) {
        let (n, from) = match sock.recv_from(&mut buf) {
            Ok(x) => x,
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => continue,
            Err(e) => return Err(e),
        };
        if answer_offer(sock, &buf[..n], from) {
            continue;
        }
        let Some(q) = body(QUERY, &buf[..n]).and_then(|b| serde_json::from_slice::<Query>(b).ok()) else { continue };
        let Ok(asker) = PeerId::from_hex(&q.from) else { continue };
        if !trusted(&asker) {
            continue;
        }
        let mut a = Announce { id: key.peer_id().to_hex(), name: name.to_string(), port, sig: String::new() };
        a.sig = hex::encode(&key.sign(&signed(&q.nonce, &a)));
        let _ = sock.send_to(&packet(ANSWER, &a), from);
    }
    Ok(())
}

/// The computer, pairing: handhelds showing a pairing code on the LAN.
pub fn find_pairing(timeout: Duration) -> io::Result<Vec<(Announce, SocketAddr)>> {
    let sock = broadcast_socket()?;
    sock.send_to(&packet(PAIR_QUERY, &Query { nonce: nonce(), from: String::new() }), (Ipv4Addr::BROADCAST, crate::PORT))?;
    let mut found: Vec<(Announce, SocketAddr)> = Vec::new();
    gather(&sock, timeout, |data, from| {
        let Some(a) = body(PAIR_ANSWER, data).and_then(|b| serde_json::from_slice::<Announce>(b).ok()) else { return };
        if PeerId::from_hex(&a.id).is_err() || found.iter().any(|(f, _)| f.id == a.id) {
            return;
        }
        let addr = SocketAddr::new(from.ip(), a.port);
        found.push((a, addr));
    })?;
    Ok(found)
}

/// Computers that can pair by comparing codes, as they answered within
/// [`timeout`]: each offer once, at the address it answered from.
pub fn find_computers(timeout: Duration) -> io::Result<Vec<(Offer, SocketAddr)>> {
    let sock = broadcast_socket()?;
    sock.send_to(&packet(COMPUTER_QUERY, &Query { nonce: nonce(), from: String::new() }), (Ipv4Addr::BROADCAST, crate::PORT))?;
    let mut found: Vec<(Offer, SocketAddr)> = Vec::new();
    gather(&sock, timeout, |data, from| {
        let Some(o) = body(COMPUTER_ANSWER, data).and_then(|b| serde_json::from_slice::<Offer>(b).ok()) else { return };
        if PeerId::from_hex(&o.id).is_err() || found.iter().any(|(f, _)| f.id == o.id) {
            return;
        }
        let addr = SocketAddr::new(from.ip(), o.port);
        found.push((o, addr));
    })?;
    Ok(found)
}

/// A computer that is not serving (nothing else holds the agent's port):
/// answers `DTCQ1` with the current offer until [`stop`].
pub fn answer_offers(sock: &UdpSocket, stop: &AtomicBool) -> io::Result<()> {
    sock.set_read_timeout(Some(Duration::from_millis(500)))?;
    let mut buf = [0u8; 2048];
    while !stop.load(Ordering::Relaxed) {
        match sock.recv_from(&mut buf) {
            Ok((n, from)) => {
                answer_offer(sock, &buf[..n], from);
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// The handheld, pairing: answers pairing queries until [`stop`].
pub fn answer_pairing(sock: &UdpSocket, me: &Announce, stop: &AtomicBool) -> io::Result<()> {
    sock.set_read_timeout(Some(Duration::from_millis(500)))?;
    let mut buf = [0u8; 2048];
    while !stop.load(Ordering::Relaxed) {
        let (n, from) = match sock.recv_from(&mut buf) {
            Ok(x) => x,
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => continue,
            Err(e) => return Err(e),
        };
        if body(PAIR_QUERY, &buf[..n]).is_some() {
            let _ = sock.send_to(&packet(PAIR_ANSWER, me), from);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    static OFFERS_SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn an_offer_is_answered_only_while_it_is_set() {
        let _serial = OFFERS_SERIAL.lock().unwrap();
        let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let asker = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        asker.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        let query = packet(COMPUTER_QUERY, &Query { nonce: nonce(), from: String::new() });
        let o = Offer { id: DeviceKey::generate().peer_id().to_hex(), name: "Desk".into(), port: 47612, program: "pythia".into() };
        offer(Some(o.clone()));
        assert!(answer_offer(&sock, &query, asker.local_addr().unwrap()));
        let mut buf = [0u8; 2048];
        let (n, _) = asker.recv_from(&mut buf).unwrap();
        assert_eq!(serde_json::from_slice::<Offer>(body(COMPUTER_ANSWER, &buf[..n]).unwrap()).unwrap(), o);
        offer(None);
        assert!(answer_offer(&sock, &query, asker.local_addr().unwrap()));
        assert!(asker.recv_from(&mut buf).is_err());
        assert!(!answer_offer(&sock, b"DTAQ1{}", asker.local_addr().unwrap()));
    }

    #[test]
    fn delayed_lease_cleanup_cannot_clear_a_newer_identical_offer_or_legacy_publication() {
        let _serial = OFFERS_SERIAL.lock().unwrap();
        let offer_value = Offer { id: "id".into(), name: "Desk".into(), port: 47612, program: "pythia".into() };
        let old = offer_lease(offer_value.clone());
        let new = offer_lease(offer_value.clone());
        drop(old);
        assert_eq!(current_offer(), Some(offer_value.clone()));
        assert!(new.withdraw());
        assert_eq!(current_offer(), None);
        drop(new);
        let old = offer_lease(offer_value.clone());
        offer(Some(offer_value.clone()));
        drop(old);
        assert_eq!(current_offer(), Some(offer_value));
        offer(None);
    }

    #[test]
    fn answers_are_signed_over_the_nonce() {
        let key = DeviceKey::generate();
        let mut a = Announce { id: key.peer_id().to_hex(), name: "PC".into(), port: 47610, sig: String::new() };
        let sig = key.sign(&signed("n1", &a));
        a.sig = hex::encode(&sig);
        assert!(verify(&key.peer_id(), &signed("n1", &a), &sig));
        assert!(!verify(&key.peer_id(), &signed("n2", &a), &sig));
    }
}
