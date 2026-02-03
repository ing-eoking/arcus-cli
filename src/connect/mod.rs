mod internal;
pub mod tcp;
pub mod udp;
pub mod unix;

use std::io;
use self::tcp::TcpClient;
use self::udp::UdpClient;
use self::unix::UnixClient;

pub trait Connection {
    fn connect(&mut self, addr: &str) -> io::Result<()>;
    fn write(&mut self, line: String) -> io::Result<()>;
    fn close(&mut self);
}

#[derive(Default)]
pub struct TransportBuilder {
    rqid: u16,
    time: u64,
    auth: bool,
}

impl TransportBuilder {
    pub fn new() -> Self { Self::default() }
    pub fn rqid(mut self, rqid: u16) -> Self { self.rqid = rqid; self }
    pub fn time(mut self, time: u64) -> Self { self.time = time; self }
    pub fn auth(mut self, auth: bool) -> Self { self.auth = auth; self }

    pub fn build_tcp(self, addr: impl Into<String>) -> Transport {
        let addr_str = addr.into();
        let mut client = TcpClient::default();
        client.auth = self.auth;
        if let Err(e) = client.connect(&addr_str) {
            eprintln!("WARNING: Failed to initial connect (TCP) to {}: {}", addr_str, e);
        }
        Transport::TCP(addr_str, client)
    }

    pub fn build_udp(self, addr: impl Into<String>) -> Transport {
        let addr_str = addr.into();
        let mut client = UdpClient::default();
        client.rqid = self.rqid;
        client.time = self.time;
        client.auth = self.auth;
        if let Err(e) = client.connect(&addr_str) {
            eprintln!("WARNING: Failed to initial setup (UDP) for {}: {}", addr_str, e);
        }
        Transport::UDP(addr_str, client)
    }

    pub fn build_unix(self, addr: impl Into<String>) -> Transport {
        let addr_str = addr.into();
        let mut client = UnixClient::default();
        client.auth = self.auth;
        if let Err(e) = client.connect(&addr_str) {
            eprintln!("WARNING: Failed to initial connect (Unix) to {}: {}", addr_str, e);
        }
        Transport::UNIX(addr_str, client)
    }
}

pub enum Transport {
    TCP(String, TcpClient),
    UDP(String, UdpClient),
    UNIX(String, UnixClient),
}

impl Transport {
    pub fn builder() -> TransportBuilder { TransportBuilder::new() }

    pub fn write(&mut self, mut line: String) {
        if !line.is_empty() && !line.ends_with('\r') { line.push('\r'); }
        if !line.ends_with('\n') { line.push('\n'); }

        match self {
            Transport::TCP (addr, c)  => Self::retry_write(c, addr, line),
            Transport::UDP (addr, c)  => Self::retry_write(c, addr, line),
            Transport::UNIX(addr, c) => Self::retry_write(c, addr, line),
        }
    }

    fn retry_write<C: Connection>(client: &mut C, addr: &str, line: String) {
        if let Err(_) = client.write(line.clone()) {
            if let Err(e) = client.connect(addr) {
                eprintln!("ERROR: Failed to reconnect to {}: {}", addr, e);
            }
        }
    }
}
