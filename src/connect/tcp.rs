use std::io::BufReader;
use std::io::prelude::*;
use std::net::{SocketAddr, TcpStream};

pub struct TcpClient {
    addr: Option<SocketAddr>,
    conn: Option<TcpStream>,
    rbuf: Option<BufReader<TcpStream>>
}

impl TcpClient {
    pub fn create() -> Self {
        TcpClient { addr: None, conn: None, rbuf: None }
    }

    pub fn connect(&mut self, address: SocketAddr) {
        self.addr = Some(address);
        match TcpStream::connect(address) {
            Ok(sock) => {
                self.rbuf = Some(BufReader::new(sock.try_clone().unwrap()));
                self.conn = Some(sock)
            },
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

    #[allow(dead_code)]
    pub fn read(&mut self) -> String {
        let mut buff = String::new();
        match self.rbuf.as_mut().unwrap().read_line(&mut buff) {
            Err(err) => eprintln!("ERROR: {}", err),
            _ => ()
        };
        return buff;
    }
}
