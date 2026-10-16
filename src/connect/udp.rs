use std::io::{self, ErrorKind, Write};
use std::net::{ToSocketAddrs, UdpSocket};
use std::time::Duration;

const HEADER_SIZE: usize = 8;
/// memcached reads a UDP request into a single 64KB buffer and rejects
/// multi-datagram requests, so one request must fit in one datagram.
const MAX_REQUEST: usize = 65507 - HEADER_SIZE;
const MAX_DATAGRAM: usize = 65535;

#[derive(Default)]
pub struct UdpClient {
    pub rqid: u16,
    pub wait: Duration,
    conn: Option<UdpSocket>,
}

impl UdpClient {
    pub fn connect(&mut self, address: &str) -> io::Result<()> {
        let addr = address.to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(ErrorKind::AddrNotAvailable, "No address found"))?;
        let bind = if addr.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" };
        let sock = UdpSocket::bind(bind)?;
        sock.connect(addr)?;
        sock.set_read_timeout(Some(self.wait.max(Duration::from_millis(1))))?;
        self.conn = Some(sock);
        Ok(())
    }

    /// Sends one request and prints its response. Requests with `noreply`
    /// get no response, so nothing is awaited for them.
    pub fn send(&mut self, data: &[u8], expect_reply: bool) -> io::Result<()> {
        let sock = self.conn.as_ref()
            .ok_or_else(|| io::Error::new(ErrorKind::NotConnected, "UDP socket not initialized"))?;
        if data.len() > MAX_REQUEST {
            return Err(io::Error::new(ErrorKind::InvalidInput,
                format!("request of {} bytes exceeds a single UDP datagram", data.len())));
        }

        self.rqid = self.rqid.wrapping_add(1);
        let rqid = self.rqid;
        let mut packet = Vec::with_capacity(HEADER_SIZE + data.len());
        packet.extend_from_slice(&rqid.to_be_bytes());
        packet.extend_from_slice(&[0, 0, 0, 1, 0, 0]); /* seq 0 of 1 */
        packet.extend_from_slice(data);
        sock.send(&packet)?;

        if !expect_reply { return Ok(()); }
        let response = Self::receive(sock, rqid)?;
        let mut out = io::stdout().lock();
        out.write_all(&response)?;
        out.flush()
    }

    fn receive(sock: &UdpSocket, rqid: u16) -> io::Result<Vec<u8>> {
        let mut buf = vec![0u8; MAX_DATAGRAM];
        let mut parts: Vec<Option<Vec<u8>>> = Vec::new();

        loop {
            let n = match sock.recv(&mut buf) {
                Ok(n) => n,
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    return Err(io::Error::new(ErrorKind::TimedOut, "UDP response timeout"));
                }
                Err(e) => return Err(e),
            };
            if n < HEADER_SIZE { continue; }
            if u16::from_be_bytes([buf[0], buf[1]]) != rqid { continue; } /* stale response */

            let seq = u16::from_be_bytes([buf[2], buf[3]]) as usize;
            let total = u16::from_be_bytes([buf[4], buf[5]]) as usize;
            if total == 0 || seq >= total { continue; }
            if parts.len() != total { parts = vec![None; total]; }
            parts[seq] = Some(buf[HEADER_SIZE..n].to_vec());

            if parts.iter().all(Option::is_some) {
                return Ok(parts.into_iter().flatten().flatten().collect());
            }
        }
    }

    pub fn close(&mut self) {
        self.conn = None;
    }
}
