use std::thread;
use std::io::BufReader;
use std::io::prelude::*;
use std::net::{SocketAddr, TcpStream, Shutdown};
use std::thread::JoinHandle;

pub struct TcpClient {
    addr: Option<SocketAddr>,
    conn: Option<TcpStream>,
    hand: Option<JoinHandle<()>>
}

impl TcpClient {
    pub fn create() -> Self {
        TcpClient { addr: None, conn: None, hand: None }
    }

    pub fn connect(&mut self, address: SocketAddr) {
        self.addr = Some(address);
        match TcpStream::connect(address) {
            Ok(sock) => {
                self.hand = self.activate_reader(sock.try_clone().unwrap());
                self.conn = Some(sock);
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

    fn activate_reader(&mut self, sock: TcpStream) -> Option<JoinHandle<()>> {
        let mut rbuf = BufReader::new(sock);
        let mut line = String::new();
        return Some(thread::spawn(move || {
            loop {
                match rbuf.read_line(&mut line) {
                    Err(err) => eprintln!("ERROR: {}", err),
                    Ok(0) => break,
                    _ => ()
                }
                print!("{}", line);
                line.clear();
            }
        }));
    }
}

impl Drop for TcpClient {
    fn drop(&mut self) {
        self.conn.as_mut().unwrap().shutdown(Shutdown::Write).unwrap();
        self.hand.take().unwrap().join().unwrap();
    }
}
