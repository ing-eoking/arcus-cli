use std::thread;
use std::io::{BufReader, ErrorKind};
use std::io::prelude::*;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::thread::JoinHandle;

#[derive(Default)]
pub struct UnixClient {
    addr: Option<String>,
    conn: Option<UnixStream>,
    hand: Option<JoinHandle<()>>,
    runn: bool,
    pub auth: bool
}

impl UnixClient {
    pub fn connect(&mut self, address: &str) {
        if !self.conn.is_none() { drop(self.conn.take().unwrap()); } /* TODO */
        else { self.addr = Some(address.to_string()) }
        self.runn = false;
        match UnixStream::connect(self.addr.as_mut().unwrap()) {
            Ok(sock) => {
                self.hand = self.activate_reader(sock.try_clone().unwrap());
                self.conn = Some(sock);
                self.runn = true;
            },
            Err(err) => {
                eprintln!("ERROR: {}", err);
                if err.kind() != ErrorKind::ConnectionRefused {
                    std::process::exit(1);
                }
            }
        };
    }

    pub fn write(&mut self, line: String) -> bool {
        return match self.conn.as_mut() {
            None => true,
            Some(conn) => matches!(
                conn.write(line.as_bytes()),
                Err(ref err) if err.kind() == ErrorKind::BrokenPipe
            ),
        };
    }

    pub fn read(&mut self) -> String {
        let mut line = String::new();
        match self.conn.as_mut() {
            None => eprintln!("ERROR: No connection"),
            Some(conn) => {
                let mut rbuf = BufReader::new(conn);
                match rbuf.read_line(&mut line) {
                    Err(e) => eprintln!("ERROR: {}", e),
                    _ => ()
                }
            }
        }
        return line;
    }

    fn activate_reader(&mut self, sock: UnixStream) -> Option<JoinHandle<()>> {
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

impl Drop for UnixClient {
    fn drop(&mut self) {
        if self.runn {
            self.conn.as_mut().unwrap().shutdown(Shutdown::Write).unwrap();
            self.hand.take().unwrap().join().unwrap();
        }
    }
}
