use std::net::{SocketAddr, UdpSocket};

pub struct UdpClient {
    rqid: u16,
    addr: Option<SocketAddr>,
    conn: Option<UdpSocket>
}

impl UdpClient {
    pub fn create() -> Self {
        UdpClient { rqid: 1, addr: None, conn: None }
    }

    pub fn connect(&mut self, address: SocketAddr, request_id: u16) {
        self.addr = Some(address);
        self.rqid = request_id;
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
}
