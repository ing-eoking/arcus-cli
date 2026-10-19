use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;
use rsasl::prelude::*;

pub(crate) fn authenticate<S>(mut stream: S) -> io::Result<S>
where S: Read + Write
{
    let mut rbuf = BufReader::new(&mut stream);

    let mut username = String::new();
    print!("username: ");
    io::stdout().flush()?;
    io::stdin().read_line(&mut username)?;
    let username = username.trim().to_string();
    let password = rpassword::prompt_password("password: ")?;

    let config = SASLConfig::with_credentials(
        None,
        username,
        password
    ).map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Config Error: {}", e)))?;

    let client = SASLClient::new(config);

    rbuf.get_mut().write_all(b"sasl mech\r\n")?;
    rbuf.get_mut().flush()?;

    let mut line = String::new();
    rbuf.read_line(&mut line)?;

    let Some(mech_list) = line.strip_prefix("SASL_MECH ") else {
        return Err(io::Error::new(io::ErrorKind::InvalidData,
                                  format!("Protocol error: {}", line.trim_end())));
    };
    let server_mechs: Vec<&Mechname> = mech_list.split_whitespace()
                                                .filter_map(|s| Mechname::parse(s.as_bytes()).ok())
                                                .collect();

    let mut session = client.start_suggested(&server_mechs)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Negotiation failed: {}", e)))?;

    let mut mech_param = Some(session.get_mechname().to_string() + " ");
    let mut input_data: Option<Vec<u8>> = None;

    loop {
        let mut out = Vec::new();
        session.step(input_data.as_deref(), &mut out)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

        let mut req = format!("sasl auth {}{}\r\n",
            mech_param.take().unwrap_or_default(),
            out.len()
        ).into_bytes();
        req.extend_from_slice(&out);
        req.extend_from_slice(b"\r\n");

        rbuf.get_mut().write_all(&req)?;
        rbuf.get_mut().flush()?;

        let mut line = String::new();
        rbuf.read_line(&mut line)?;

        if let Some(len) = line.strip_prefix("SASL_CONTINUE ") {
            let len: usize = len.trim_end().parse()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Protocol error: bad SASL_CONTINUE"))?;
            let mut data = vec![0u8; len + 2];
            rbuf.read_exact(&mut data)?;
            data.truncate(len);
            input_data = Some(data);
        } else if line == "SASL_OK\r\n" {
            break;
        } else {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("Auth failed: {}", line.trim())));
        }
    }

    Ok(stream)
}

/// A bidirectional byte stream (TCP or Unix domain socket).
pub(crate) trait Stream: Read + Write + Send + Sized + 'static {
    fn open(addr: &str) -> io::Result<Self>;
    fn try_clone(&self) -> io::Result<Self>;
    fn shutdown(&self, how: Shutdown) -> io::Result<()>;
}

/// Writes requests to a stream while a background thread copies every
/// response byte to stdout as it arrives.
pub(crate) struct StreamClient<S: Stream> {
    pub auth: bool,
    conn: Option<S>,
    done: Option<Receiver<()>>,
}

impl<S: Stream> Default for StreamClient<S> {
    fn default() -> Self { Self { auth: false, conn: None, done: None } }
}

impl<S: Stream> StreamClient<S> {
    pub fn connect(&mut self, addr: &str) -> io::Result<()> {
        self.close();
        let stream = S::open(addr)?;
        let stream = if self.auth { authenticate(stream)? } else { stream };

        let mut reader = stream.try_clone()?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            let stdout = io::stdout();
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut out = stdout.lock();
                        let _ = out.write_all(&buf[..n]);
                        let _ = out.flush();
                    }
                }
            }
            let _ = tx.send(());
        });
        self.done = Some(rx);
        self.conn = Some(stream);
        Ok(())
    }

    pub fn send(&mut self, data: &[u8]) -> io::Result<()> {
        if self.is_closed() { self.conn = None; }
        match &mut self.conn {
            Some(conn) => conn.write_all(data).and_then(|_| conn.flush()),
            None => Err(io::Error::new(io::ErrorKind::NotConnected, "No connection")),
        }
    }

    /// The server closed the connection (e.g. after `quit`).
    fn is_closed(&self) -> bool {
        match &self.done {
            Some(rx) => !matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
            None => true,
        }
    }

    /// Stops sending and waits up to `wait` for the remaining responses.
    pub fn finish(&mut self, wait: Duration) {
        if let Some(conn) = self.conn.as_ref() {
            let _ = conn.shutdown(Shutdown::Write);
        }
        if let Some(rx) = self.done.take() {
            let _ = rx.recv_timeout(wait);
        }
        self.close();
    }

    pub fn close(&mut self) {
        if let Some(conn) = self.conn.take() {
            let _ = conn.shutdown(Shutdown::Both);
        }
        if let Some(rx) = self.done.take() {
            let _ = rx.recv();
        }
    }
}
