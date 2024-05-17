pub mod tcp;

use std::net::{ToSocketAddrs};
use self::tcp::TcpClient;

pub enum Transport {
    TCP,
    UDP,
    UNIX
}

pub struct Conn {
    transport: Transport,
    tcp: TcpClient,
}

impl Conn {
    pub fn create() -> Self {
        Conn {
            transport: Transport::TCP,
            tcp: tcp::TcpClient::create(),
        }
    }
    pub fn connect(&mut self, host: String, port: u32, prot:Transport) {
        let addrs_iter = format!("{}:{}", host, port).to_socket_addrs();
        let mut addrs_iter = match addrs_iter {
            Ok(addr) => addr,
            Err(err) => {
                eprintln!("ERROR: {}", err);
                std::process::exit(1);
            }
        };
        self.transport = prot;
        match self.transport {
            Transport::TCP => self.tcp.connect(addrs_iter.next().unwrap()),
            Transport::UDP => (),
            Transport::UNIX => ()
        }
    }

    pub fn write(&mut self, line: String) {
        let mut buf = line;
        if buf.len() > 0 && &buf[buf.len() - 1..] != "\r" { buf.push('\r'); }
        buf.push('\n');
        match self.transport {
            Transport::TCP => self.tcp.write(buf),
            Transport::UDP => (),
            Transport::UNIX => ()
        }
    }
}
