use std::thread;
use std::io::{BufReader, ErrorKind};
use std::io::prelude::*;
use std::net::{SocketAddr, TcpStream, Shutdown, ToSocketAddrs};
use std::thread::JoinHandle;

#[derive(Default)]
pub struct TcpClient {
    addr: Option<SocketAddr>,
    conn: Option<TcpStream>,
    hand: Option<JoinHandle<()>>,
    down: bool
}

impl TcpClient {
    pub fn connect(&mut self, address: &str) {
        if !self.conn.is_none() { drop(self.conn.take().unwrap()) }
        let addrs_iter = if self.addr.is_none() {
            match address.to_socket_addrs() {
                Ok(addrs) => addrs.collect::<Vec<_>>(),
                Err(err) => {
                    eprintln!("ERROR: {}", err);
                    std::process::exit(1);
                }
            }
        } else {
            vec![self.addr.unwrap()]
        };

        let mut last_err = None;
        for addr in addrs_iter {
            match TcpStream::connect(addr) {
                Ok(stream) => {
                    self.hand = self.activate_reader(stream.try_clone().unwrap());
                    self.conn = Some(stream);
                    self.addr = Some(addr); /* No need */
                    return;
                },
                Err(e) => last_err = Some(e)
            }
        };

        let err = last_err.unwrap();
        eprintln!("ERROR: {}", err);
        if err.kind() != ErrorKind::ConnectionRefused {
            std::process::exit(1);
        }
    }

    pub fn write(&mut self, line: String) {
        let should_connect = match self.conn.as_mut() {
            None => true,
            Some(conn) => matches!(
                conn.write(line.as_bytes()),
                Err(ref err) if err.kind() == ErrorKind::BrokenPipe
            ),
        };

        if should_connect {
            self.connect("");
        }
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
        if !self.down {
            self.conn.as_mut().unwrap().shutdown(Shutdown::Write).unwrap();
            self.hand.take().unwrap().join().unwrap();
        }
    }
}
