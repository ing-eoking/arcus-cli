use std::io::prelude::*;
use std::net::{SocketAddr, TcpStream};

pub struct TcpClient {
    addr: Option<SocketAddr>,
    conn: Option<TcpStream>
}

impl TcpClient {
    pub fn create() -> Self {
        TcpClient { addr: None, conn: None }
    }

    pub fn connect(&mut self, address: SocketAddr) {
        self.addr = Some(address);
        self.conn = match TcpStream::connect(address) {
            Ok(sock) => Some(sock),
            Err(err) => {
                eprintln!("ERROR: {}", err);
                std::process::exit(1);
            }
        };
    }

    pub fn write(&mut self, line: String) {
        match self.conn.as_mut().unwrap().write(line.as_bytes()) {
            Err(err) => eprintln!("ERROR: {}", err),
            _ => ()
        };
    }
}
