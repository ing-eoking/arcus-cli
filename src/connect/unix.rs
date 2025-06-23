use std::thread;
use std::io::BufReader;
use std::io::prelude::*;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::thread::JoinHandle;

#[derive(Default)]
pub struct UnixClient {
    addr: Option<String>,
    conn: Option<UnixStream>,
    hand: Option<JoinHandle<()>>,
    down: bool
}

impl UnixClient {
    pub fn connect(&mut self, address: &str) {
        self.addr = Some(address.to_string());
        self.down = false;
        match UnixStream::connect(self.addr.as_mut().unwrap()) {
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
        if !self.down {
            self.conn.as_mut().unwrap().shutdown(Shutdown::Write).unwrap();
            self.hand.take().unwrap().join().unwrap();
        }
    }
}
