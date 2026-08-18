//! Peer-UID filtering for the loopback listener.
//!
//! A loopback port is reachable by every local process, so when the daemon
//! runs as one human's user service the sensible boundary is "only that
//! user". Linux has no `SO_PEERCRED` for TCP, but a loopback connection's
//! client socket appears in `/proc/net/tcp{,6}` with its owner's uid, keyed
//! by the address pair — which is exactly what the server already knows.

use std::io;
use std::net::SocketAddr;

use tokio::net::{TcpListener, TcpStream};

/// Wraps a listener, dropping connections from uids that are not allowed.
pub struct UidFiltered {
    inner: TcpListener,
    allowed: Vec<u32>,
}

impl UidFiltered {
    /// `allowed` empty means no filtering.
    pub fn new(inner: TcpListener, allowed: Vec<u32>) -> Self {
        Self { inner, allowed }
    }

    fn permitted(&self, local: SocketAddr, peer: SocketAddr) -> bool {
        if self.allowed.is_empty() {
            return true;
        }
        match peer_uid(local, peer) {
            Some(uid) => {
                let ok = self.allowed.contains(&uid);
                if !ok {
                    tracing::warn!(%peer, uid, "rejected connection from another user");
                }
                ok
            }
            // Fail closed: an unidentifiable peer is refused rather than
            // silently granted the credentials this daemon holds.
            None => {
                tracing::warn!(%peer, "rejected connection with unidentifiable owner");
                false
            }
        }
    }
}

impl axum::serve::Listener for UidFiltered {
    type Io = TcpStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let Ok((stream, peer)) = self.inner.accept().await else {
                // Transient accept errors (EMFILE and friends): retry rather
                // than tear down the listener.
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                continue;
            };
            let local = stream.local_addr().unwrap_or(peer);
            if self.permitted(local, peer) {
                return (stream, peer);
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.inner.local_addr()
    }
}

/// The uid of the process owning the other end of this connection, found by
/// locating the peer's socket in the kernel's TCP table.
pub fn peer_uid(local: SocketAddr, peer: SocketAddr) -> Option<u32> {
    let table = if peer.is_ipv4() { "/proc/net/tcp" } else { "/proc/net/tcp6" };
    let text = std::fs::read_to_string(table).ok()?;
    // From the peer's point of view the address pair is reversed.
    let want_local = hex_addr(peer)?;
    let want_remote = hex_addr(local)?;
    for line in text.lines().skip(1) {
        let mut fields = line.split_whitespace();
        let (Some(_sl), Some(l), Some(r)) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        if !l.eq_ignore_ascii_case(&want_local) || !r.eq_ignore_ascii_case(&want_remote) {
            continue;
        }
        // sl local rem st tx:rx tr:when retrnsmt uid
        return fields.nth(4)?.parse().ok();
    }
    None
}

/// `/proc/net/tcp` spells addresses as host-endian hex words followed by the
/// port: `0100007F:1F90` for 127.0.0.1:8080 on a little-endian machine.
fn hex_addr(addr: SocketAddr) -> Option<String> {
    let words: Vec<u32> = match addr {
        SocketAddr::V4(v4) => vec![u32::from_le_bytes(v4.ip().octets())],
        SocketAddr::V6(v6) => v6
            .ip()
            .octets()
            .chunks(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    };
    let hex: String = words.iter().map(|w| format!("{w:08X}")).collect();
    Some(format!("{hex}:{:04X}", addr.port()))
}

/// This process's real uid, read from `/proc/self/status`.
pub fn own_uid() -> Option<u32> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_addresses_the_way_proc_does() {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        assert_eq!(hex_addr(addr).unwrap(), "0100007F:1F90");
    }

    #[test]
    fn own_uid_is_readable() {
        assert!(own_uid().is_some(), "/proc/self/status should expose our uid");
    }

    /// The lookup has to work against the real kernel table, so make a real
    /// loopback connection and check we recognise ourselves.
    #[tokio::test]
    async fn identifies_the_owner_of_a_live_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).await.unwrap();
        let (server, peer) = listener.accept().await.unwrap();

        let uid = peer_uid(server.local_addr().unwrap(), peer);
        assert_eq!(uid, own_uid(), "a connection from this process must resolve to our uid");
        drop((client, server));
    }
}
