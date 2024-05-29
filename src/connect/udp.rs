use std::net::{SocketAddr, UdpSocket};


static MTU: usize = 1400;
pub struct UdpClient {
    rqid: u16,
    addr: Option<SocketAddr>,
    conn: Option<UdpSocket>,
    time: u64
}

impl UdpClient {
    pub fn create() -> Self {
        UdpClient { rqid: 1, addr: None, conn: None, time: 0 }
    }

    pub fn connect(&mut self, address: SocketAddr, request_id: u16, timeout: u64) {
        self.addr = Some(address);
        self.rqid = request_id;
        self.time = timeout;
        match UdpSocket::bind("127.0.0.1:0") {
            Ok(sock) => {
                self.conn = Some(sock);
            }
            Err(err) => {
                eprintln!("ERROR: {}", err);
                std::process::exit(1);
            }
        };
    }

    pub fn write(&mut self, line: String) {
        let msg = self.build_header(line);
        for m in msg {
            match self.conn.as_mut().unwrap().send_to(&m, self.addr.unwrap()) {
                Err(err) => { eprintln!("ERROR: {}", err); break; }
                _ => ()
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
            let last     = if i*MTU + 1400 > line.len() { line.len() }
                              else { i*MTU + 1400 };
            let part = line[i*MTU..last].as_bytes();
            ret.push([buffer, part.try_into().unwrap()].concat());
        }
        return ret;
    }
}
