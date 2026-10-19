mod internal;
pub mod tcp;
pub mod udp;
pub mod unix;

use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use crate::request::Request;
use self::internal::{Stream, StreamClient};
use self::tcp::TcpClient;
use self::udp::UdpClient;
use self::unix::UnixClient;

/// Where response bytes go; the REPL uses stdout, MCP tools use a buffer.
pub type Sink = Arc<Mutex<Box<dyn Write + Send>>>;

pub fn stdout_sink() -> Sink { Arc::new(Mutex::new(Box::new(io::stdout()))) }

#[derive(Default)]
pub struct TransportBuilder {
    rqid: u16,
    wait: Duration,
    auth: bool,
    sink: Option<Sink>,
}

impl TransportBuilder {
    pub fn new() -> Self { Self::default() }
    pub fn rqid(mut self, rqid: u16) -> Self { self.rqid = rqid; self }
    pub fn wait(mut self, wait: Duration) -> Self { self.wait = wait; self }
    pub fn auth(mut self, auth: bool) -> Self { self.auth = auth; self }
    pub fn sink(mut self, sink: Sink) -> Self { self.sink = Some(sink); self }

    pub fn build_tcp(self, addr: impl Into<String>) -> Transport {
        let addr = addr.into();
        let mut client = TcpClient::default();
        client.auth = self.auth;
        if let Some(sink) = &self.sink { client.sink = sink.clone(); }
        if let Err(e) = client.connect(&addr) {
            eprintln!("WARNING: Failed to initial connect (TCP) to {}: {}", addr, e);
        }
        Transport { addr, wait: self.wait, kind: Kind::Tcp(client) }
    }

    pub fn build_udp(self, addr: impl Into<String>) -> Transport {
        let addr = addr.into();
        if self.auth {
            eprintln!("WARNING: SASL authentication is not supported over UDP");
        }
        let mut client = UdpClient::default();
        /* send() increments it before use */
        client.rqid = self.rqid.wrapping_sub(1);
        client.wait = self.wait;
        if let Some(sink) = &self.sink { client.sink = sink.clone(); }
        if let Err(e) = client.connect(&addr) {
            eprintln!("WARNING: Failed to initial setup (UDP) for {}: {}", addr, e);
        }
        Transport { addr, wait: self.wait, kind: Kind::Udp(client) }
    }

    pub fn build_unix(self, addr: impl Into<String>) -> Transport {
        let addr = addr.into();
        let mut client = UnixClient::default();
        client.auth = self.auth;
        if let Some(sink) = &self.sink { client.sink = sink.clone(); }
        if let Err(e) = client.connect(&addr) {
            eprintln!("WARNING: Failed to initial connect (Unix) to {}: {}", addr, e);
        }
        Transport { addr, wait: self.wait, kind: Kind::Unix(client) }
    }
}

enum Kind {
    Tcp(TcpClient),
    Udp(UdpClient),
    Unix(UnixClient),
}

pub struct Transport {
    addr: String,
    wait: Duration,
    kind: Kind,
}

impl Transport {
    pub fn builder() -> TransportBuilder { TransportBuilder::new() }

    pub fn send(&mut self, req: &Request) {
        if let Err(e) = self.try_send(req) {
            eprintln!("ERROR: {}", e);
        }
    }

    pub fn try_send(&mut self, req: &Request) -> io::Result<()> {
        let data = req.to_wire();
        let addr = self.addr.as_str();
        match &mut self.kind {
            Kind::Tcp(c) => Self::send_stream(c, addr, &data),
            Kind::Unix(c) => Self::send_stream(c, addr, &data),
            Kind::Udp(c) => c.send(&data, !req.is_noreply()),
        }
    }

    /// Sends, reconnecting and sending again once if the connection is gone.
    fn send_stream<S: Stream>(c: &mut StreamClient<S>, addr: &str, data: &[u8]) -> io::Result<()> {
        if c.send(data).is_ok() { return Ok(()); }
        c.connect(addr)
            .map_err(|e| io::Error::new(e.kind(), format!("Failed to reconnect to {}: {}", addr, e)))?;
        c.send(data)
            .map_err(|e| io::Error::new(e.kind(), format!("Failed to send to {}: {}", addr, e)))
    }

    /// Waits for the responses still in flight, then closes.
    pub fn finish(&mut self) {
        match &mut self.kind {
            Kind::Tcp(c) => c.finish(self.wait),
            Kind::Unix(c) => c.finish(self.wait),
            Kind::Udp(c) => c.close(),
        }
    }
}

/// A sink that accumulates bytes and remembers when the last ones arrived.
struct Capture(Arc<Mutex<(Vec<u8>, Instant)>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut g = self.0.lock().map_err(|_| io::Error::other("capture poisoned"))?;
        g.0.extend_from_slice(buf);
        g.1 = Instant::now();
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

/// Sends every request in `input` over TCP/Unix and returns the response text.
/// Returns once nothing new arrived for `idle` (or `max_wait` / `max_bytes` is hit,
/// which is noted at the end of the text).
pub fn collect_responses(addr: &str, unix: bool, input: &str, idle: Duration,
                         max_wait: Duration, max_bytes: usize) -> io::Result<String> {
    let split = crate::request::split(input);
    if let Some(first) = split.pending.first() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput,
            format!("incomplete request `{first}`: the data block is missing or has the wrong length")));
    }
    if split.requests.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "no command given"));
    }

    let shared = Arc::new(Mutex::new((Vec::new(), Instant::now())));
    let sink: Sink = Arc::new(Mutex::new(Box::new(Capture(shared.clone()))));
    let builder = Transport::builder().wait(Duration::from_millis(50)).sink(sink);
    let mut transport = if unix { builder.build_unix(addr) } else { builder.build_tcp(addr) };

    for req in &split.requests {
        transport.try_send(req)?;
    }

    let start = Instant::now();
    let mut note = "";
    loop {
        std::thread::sleep(Duration::from_millis(10));
        let (len, last) = { let g = shared.lock().unwrap(); (g.0.len(), g.1) };
        let quiet_since = if len == 0 { start } else { last.max(start) };
        if len >= max_bytes { note = "output exceeded the size limit"; break; }
        if quiet_since.elapsed() >= idle { break; }
        if start.elapsed() >= max_wait { note = "stopped at the maximum wait time"; break; }
    }
    transport.finish();

    let mut bytes = std::mem::take(&mut shared.lock().unwrap().0);
    if bytes.len() > max_bytes { bytes.truncate(max_bytes); }
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if !note.is_empty() { text.push_str(&format!("\n[truncated: {note}]")); }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn ms(n: u64) -> Duration { Duration::from_millis(n) }

    fn fake_server() -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = [0u8; 64];
            let mut got = Vec::new();
            while !got.ends_with(b"get a\r\n") {
                let n = s.read(&mut buf).unwrap();
                if n == 0 { return; }
                got.extend_from_slice(&buf[..n]);
            }
            s.write_all(b"END\r\n").unwrap();
            let _ = s.read(&mut buf); /* hold open until the client closes */
        });
        addr
    }

    #[test]
    fn collect_from_fake_server() {
        let addr = fake_server();
        assert_eq!(collect_responses(&addr, false, "get a", ms(100), ms(2000), 1 << 20).unwrap(), "END\r\n");
    }

    #[test]
    fn collect_rejects_incomplete() {
        let addr = fake_server();
        let e = collect_responses(&addr, false, "set k 0 0 5", ms(100), ms(500), 1024).unwrap_err();
        assert!(e.to_string().contains("set k 0 0 5"), "{e}");
    }

    #[test]
    fn collect_connect_failure_is_error() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        drop(l);
        assert!(collect_responses(&addr, false, "get a", ms(50), ms(500), 1024).is_err());
    }
}
