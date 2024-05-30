use std::io;
use std::time::Duration;
use std::net::{SocketAddr, UdpSocket};


const MTU: usize = 1400;
pub struct UdpClient {
    rqid: u16,
    addr: Option<SocketAddr>,
    conn: Option<UdpSocket>,
    sync: bool,
}

impl UdpClient {
    pub fn create() -> Self {
        UdpClient { rqid: 1, addr: None, conn: None, sync: false }
    }

    pub fn connect(&mut self, address: SocketAddr, request_id: u16, timeout: u64) {
        self.addr = Some(address);
        self.rqid = request_id;
        let timeout = Some(Duration::from_millis(timeout));
        match UdpSocket::bind("127.0.0.1:0") {
            Ok(sock) => {
                match sock.set_read_timeout(timeout) {
                    Err(err) => {
                        eprintln!("ERROR: {}", err);
                        std::process::exit(1);
                    }
                    _ => ()
                }
                self.conn = Some(sock);
            }
            Err(err) => {
                eprintln!("ERROR: {}", err);
                std::process::exit(1);
            }
        };
    }

    pub fn write(&mut self, line: String) {
        let msg = if self.sync { self.split_message(line) }
                      else { self.build_header(line) };
        for m in msg {
            match self.conn.as_mut().unwrap().send_to(&m, self.addr.unwrap()) {
                Err(err) => { eprintln!("ERROR: {}", err); break; }
                _ => ()
            }
            let mut buf = [0; MTU+100];
            match self.conn.as_mut().unwrap().recv_from(&mut buf) {
                Err(err) => {
                    if err.kind() == io::ErrorKind::Interrupted {
                        self.sync = true;
                    }
                    else { eprintln!("ERROR: {}", err); break; }
                }
                _ => {
                    self.sync = false;
                    break;
                }
            }
        }
    }

    fn build_header(&mut self, line: String) -> Vec<Vec<u8>> {
        let mut ret = Vec::new();
        let mut buffer: [u8; 8] = [0; 8];
        let split = (line.len() + (MTU - 1)) / MTU;
        buffer[0] = (self.rqid / 255) as u8;
        buffer[1] = (self.rqid % 255) as u8;
        buffer[4] = (split / 255) as u8;
        buffer[5] = (split % 255) as u8;
        for i in 0..split {
            buffer[2] = (i / 255) as u8;
            buffer[3] = (i % 255) as u8;
            let last     = if (i + 1) * MTU > line.len() { line.len() }
                              else { (i + 1) * MTU };
            ret.push([&buffer, line[i*MTU..last].as_bytes()].concat());
        }
        return ret;
    }

    fn split_message(&mut self, line: String) -> Vec<Vec<u8>> {
        let mut ret = Vec::new();
        let split = (line.len() + (MTU - 1)) / MTU;
        for i in 0..split {
            let last= if (i + 1) * MTU > line.len() { line.len() }
                              else { (i + 1) * MTU };
            ret.push(line[i*MTU..last].as_bytes().to_vec());
        }
        return ret;
    }
}
