mod internal;
pub mod tcp;
pub mod udp;
pub mod unix;

use std::time::Duration;
use crate::request::Request;
use self::internal::{Stream, StreamClient};
use self::tcp::TcpClient;
use self::udp::UdpClient;
use self::unix::UnixClient;

#[derive(Default)]
pub struct TransportBuilder {
    rqid: u16,
    wait: Duration,
    auth: bool,
}

impl TransportBuilder {
    pub fn new() -> Self { Self::default() }
    pub fn rqid(mut self, rqid: u16) -> Self { self.rqid = rqid; self }
    pub fn wait(mut self, wait: Duration) -> Self { self.wait = wait; self }
    pub fn auth(mut self, auth: bool) -> Self { self.auth = auth; self }

    pub fn build_tcp(self, addr: impl Into<String>) -> Transport {
        let addr = addr.into();
        let mut client = TcpClient::default();
        client.auth = self.auth;
        if let Err(e) = client.connect(&addr) {
            eprintln!("WARNING: Failed to initial connect (TCP) to {}: {}", addr, e);
        }
        Transport { addr, wait: self.wait, kind: Kind::Tcp(client) }
    }

    pub fn build_udp(self, addr: impl Into<String>) -> Transport {
        let addr = addr.into();
        if self.auth {
            eprintln!("WARNING: SASL authentication is not supported over UDP");
        }
        let mut client = UdpClient::default();
        /* send() increments it before use */
        client.rqid = self.rqid.wrapping_sub(1);
        client.wait = self.wait;
        if let Err(e) = client.connect(&addr) {
            eprintln!("WARNING: Failed to initial setup (UDP) for {}: {}", addr, e);
        }
        Transport { addr, wait: self.wait, kind: Kind::Udp(client) }
    }

    pub fn build_unix(self, addr: impl Into<String>) -> Transport {
        let addr = addr.into();
        let mut client = UnixClient::default();
        client.auth = self.auth;
        if let Err(e) = client.connect(&addr) {
            eprintln!("WARNING: Failed to initial connect (Unix) to {}: {}", addr, e);
        }
        Transport { addr, wait: self.wait, kind: Kind::Unix(client) }
    }
}

enum Kind {
    Tcp(TcpClient),
    Udp(UdpClient),
    Unix(UnixClient),
}

pub struct Transport {
    addr: String,
    wait: Duration,
    kind: Kind,
}

impl Transport {
    pub fn builder() -> TransportBuilder { TransportBuilder::new() }

    pub fn send(&mut self, req: &Request) {
        let data = req.to_wire();
        let addr = self.addr.as_str();
        match &mut self.kind {
            Kind::Tcp(c) => Self::send_stream(c, addr, &data),
            Kind::Unix(c) => Self::send_stream(c, addr, &data),
            Kind::Udp(c) => {
                if let Err(e) = c.send(&data, !req.is_noreply()) {
                    eprintln!("ERROR: {}", e);
                }
            }
        }
    }

    /// Sends, reconnecting and sending again once if the connection is gone.
    fn send_stream<S: Stream>(c: &mut StreamClient<S>, addr: &str, data: &[u8]) {
        if c.send(data).is_ok() { return; }
        if let Err(e) = c.connect(addr) {
            eprintln!("ERROR: Failed to reconnect to {}: {}", addr, e);
        } else if let Err(e) = c.send(data) {
            eprintln!("ERROR: Failed to send to {}: {}", addr, e);
        }
    }

    /// Waits for the responses still in flight, then closes.
    pub fn finish(&mut self) {
        match &mut self.kind {
            Kind::Tcp(c) => c.finish(self.wait),
            Kind::Unix(c) => c.finish(self.wait),
            Kind::Udp(c) => c.close(),
        }
    }
}
