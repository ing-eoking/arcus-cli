use std::io::{self, ErrorKind};
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::Duration;
use super::Connection;

const MTU: usize = 1400;
const HEADER_SIZE: usize = 8;

#[derive(Default)]
pub struct UdpClient {
    pub rqid: u16,
    pub time: u64,
    pub auth: bool,
    addr: Option<SocketAddr>,
    conn: Option<UdpSocket>,
    sync: bool,
}

impl Connection for UdpClient {
    fn connect(&mut self, address: &str) -> io::Result<()> {
        let addr = address.to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(ErrorKind::AddrNotAvailable, "No address found"))?;

        self.addr = Some(addr);

        let sock = UdpSocket::bind("0.0.0.0:0")?;

        if self.time > 0 {
            let timeout = Duration::from_millis(self.time);
            sock.set_read_timeout(Some(timeout))?;
            sock.set_write_timeout(Some(timeout))?;
        }

        self.conn = Some(sock);
        Ok(())
    }

    fn write(&mut self, line: String) -> io::Result<()> {
        let sock = self.conn.as_ref()
            .ok_or_else(|| io::Error::new(ErrorKind::NotConnected, "UDP socket not initialized"))?;
        let addr = self.addr
            .ok_or_else(|| io::Error::new(ErrorKind::AddrNotAvailable, "UDP target address not set"))?;

        let msgs = if self.sync {
            self.split_message(&line)
        } else {
            self.build_header(&line)
        };

        let mut buf = [0u8; MTU];

        for msg in msgs {
            sock.send_to(&msg, addr)?;

            match sock.recv_from(&mut buf) {
                Ok(_) => {
                    self.sync = false;
                }
                Err(ref e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                    self.sync = !self.sync;
                    return Err(io::Error::new(ErrorKind::TimedOut, "UDP Receive Timeout"));
                }
                Err(e) => return Err(e),
            }
        }

        if !self.sync {
            if self.reassemble_response(sock, &buf) {
                Ok(())
            } else {
                Err(io::Error::new(ErrorKind::InvalidData, "Invalid UDP fragmented response"))
            }
        } else {
            Ok(())
        }
    }

    fn close(&mut self) {
        self.conn = None;
    }
}

impl UdpClient {
    fn build_header(&self, line: &str) -> Vec<Vec<u8>> {
        let payload_size = MTU - HEADER_SIZE;
        let line_bytes = line.as_bytes();
        let split_count = (line_bytes.len() + payload_size - 1) / payload_size;
        let mut ret = Vec::new();

        for i in 0..split_count {
            let start = i * payload_size;
            let end = std::cmp::min(start + payload_size, line_bytes.len());

            let mut packet = vec![0u8; HEADER_SIZE];
            packet[0] = (self.rqid / 256) as u8;
            packet[1] = (self.rqid % 256) as u8;
            packet[2] = (i / 256) as u8;
            packet[3] = (i % 256) as u8;
            packet[4] = (split_count / 256) as u8;
            packet[5] = (split_count % 256) as u8;
            packet.extend_from_slice(&line_bytes[start..end]);
            ret.push(packet);
        }
        ret
    }

    fn split_message(&self, line: &str) -> Vec<Vec<u8>> {
        line.as_bytes()
            .chunks(MTU)
            .map(|chunk| chunk.to_vec())
            .collect()
    }

    fn reassemble_response(&self, sock: &UdpSocket, first_pkt: &[u8]) -> bool {
        if first_pkt.len() < HEADER_SIZE { return false; }

        let total = (256 * first_pkt[4] as usize) + first_pkt[5] as usize;
        let mut buckets = vec![Vec::new(); total];

        let seq = (256 * first_pkt[2] as usize) + first_pkt[3] as usize;
        if seq < total {
            buckets[seq] = first_pkt[HEADER_SIZE..].to_vec();
        }

        let mut buf = [0u8; MTU];
        for _ in 1..total {
             if let Ok((size, _)) = sock.recv_from(&mut buf) {
                 let s = (256 * buf[2] as usize) + buf[3] as usize;
                 if s < total {
                     buckets[s] = buf[HEADER_SIZE..size].to_vec();
                 }
             } else {
                 return false;
             }
        }

        let flat_data: Vec<u8> = buckets.into_iter().flatten().collect();
        match String::from_utf8(flat_data) {
            Ok(s) => {
                print!("{}", s);
                true
            },
            Err(_) => {
                eprintln!("ERROR: Received non-UTF8 UDP response");
                false
            }
        }
    }
}
