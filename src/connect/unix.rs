use std::os::unix::net::UnixStream;
use std::net::Shutdown;
use std::thread::JoinHandle;
use std::io::{self, Write};
use super::internal::{authenticate, spawn_reader_loop};
use super::Connection;

#[derive(Default)]
pub struct UnixClient {
    pub auth: bool,
    conn: Option<UnixStream>,
    hand: Option<JoinHandle<()>>,
}

impl Connection for UnixClient {
    fn connect(&mut self, path: &str) -> io::Result<()> {
        self.close();
        let stream = UnixStream::connect(path)?;
        let stream = if self.auth {
            authenticate(stream)?
        } else {
            stream
        };

        let read_stream = stream.try_clone()?;
        self.hand = Some(spawn_reader_loop(read_stream));
        self.conn = Some(stream);
        Ok(())
    }

    fn write(&mut self, line: String) -> io::Result<()> {
        match &mut self.conn {
            Some(conn) => conn.write_all(line.as_bytes()),
            None => Err(io::Error::new(io::ErrorKind::NotConnected, "Unix No connection")),
        }
    }

    fn close(&mut self) {
        if let Some(conn) = self.conn.as_mut() {
            let _ = conn.shutdown(Shutdown::Both);
        }
        self.hand.take();
        self.conn = None;
    }
}
