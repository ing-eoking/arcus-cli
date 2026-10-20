use serde_json::Value;
use sha2::{Digest, Sha256, Sha512};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Expected digest of a downloaded file (lowercase hex).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checksum { Sha256(String), Sha512(String) }

#[derive(Debug, Clone)]
pub struct Request { pub url: String, pub headers: Vec<(String, String)> }

enum Hasher { S256(Sha256), S512(Sha512) }

impl Hasher {
    fn new(sum: &Checksum) -> Hasher {
        match sum { Checksum::Sha256(_) => Hasher::S256(Sha256::new()), Checksum::Sha512(_) => Hasher::S512(Sha512::new()) }
    }
    fn update(&mut self, data: &[u8]) {
        match self { Hasher::S256(h) => h.update(data), Hasher::S512(h) => h.update(data) }
    }
    fn finish_hex(self) -> String {
        let bytes: Vec<u8> = match self { Hasher::S256(h) => h.finalize().to_vec(), Hasher::S512(h) => h.finalize().to_vec() };
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}

fn expected(sum: &Checksum) -> &str {
    match sum { Checksum::Sha256(s) | Checksum::Sha512(s) => s }
}

pub fn file_digest_matches(path: &Path, sum: &Checksum) -> io::Result<bool> {
    let mut f = fs::File::open(path)?;
    let mut h = Hasher::new(sum);
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 { break; }
        h.update(&buf[..n]);
    }
    Ok(h.finish_hex().eq_ignore_ascii_case(expected(sum)))
}

/// URL without scheme-independent secrets: query string and fragment stripped.
fn safe_url(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

/// Connect timeout for every request.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Time allowed for the server to start answering (headers) and to accept the request.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
/// Overall limit for small JSON API calls (they run on the MCP request path).
const JSON_TIMEOUT: Duration = Duration::from_secs(20);

/// `global` bounds the whole call (JSON API calls); file downloads pass `None` and are
/// protected only by the connect/response timeouts so large files can finish.
fn agent(global: Option<Duration>) -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_send_request(Some(RESPONSE_TIMEOUT))
        .timeout_recv_response(Some(RESPONSE_TIMEOUT))
        .timeout_global(global)
        .build()
        .into()
}

/// Sends a GET; returns the response for a 2xx status, otherwise an error
/// that mentions only the URL path (no headers, no query string).
fn send(req: &Request, global: Option<Duration>) -> io::Result<ureq::http::Response<ureq::Body>> {
    let mut r = agent(global).get(&req.url)
        .header("User-Agent", concat!("arcus-cli/", env!("CARGO_PKG_VERSION")));
    for (k, v) in &req.headers { r = r.header(k.as_str(), v.as_str()); }
    let url = safe_url(&req.url);
    let resp = r.call().map_err(|e| io::Error::other(format!("GET {url} failed: {}", transport_reason(&e))))?;
    let code = resp.status().as_u16();
    if (200..300).contains(&code) { return Ok(resp); }
    let kind = match code {
        401 | 403 => io::ErrorKind::PermissionDenied,
        404 => io::ErrorKind::NotFound,
        _ => io::ErrorKind::Other,
    };
    Err(io::Error::new(kind, format!("GET {url} failed: HTTP {code}")))
}

/// Error description with any URL (which could carry a query string) removed.
fn transport_reason(e: &ureq::Error) -> String {
    match e {
        ureq::Error::Io(io) => io.kind().to_string(),
        ureq::Error::Timeout(_) => "timeout".to_string(),
        ureq::Error::HostNotFound => "host not found".to_string(),
        ureq::Error::ConnectionFailed => "connection failed".to_string(),
        ureq::Error::Tls(_) | ureq::Error::Rustls(_) => "TLS error".to_string(),
        _ => "request error".to_string(),
    }
}

pub fn fetch_cached(req: &Request, dest: &Path, sum: Option<&Checksum>) -> io::Result<PathBuf> {
    if dest.exists() {
        match sum {
            None => return Ok(dest.to_path_buf()),
            Some(s) => {
                if file_digest_matches(dest, s)? { return Ok(dest.to_path_buf()); }
                fs::remove_file(dest)?;
            }
        }
    }
    if let Some(parent) = dest.parent() { fs::create_dir_all(parent)?; }
    let mut part_name = dest.as_os_str().to_owned();
    part_name.push(".part");
    let part = PathBuf::from(part_name);

    let result = download(req, &part, sum);
    if let Err(e) = result {
        let _ = fs::remove_file(&part);
        return Err(e);
    }
    if let Err(e) = fs::rename(&part, dest) {
        let _ = fs::remove_file(&part);
        return Err(e);
    }
    Ok(dest.to_path_buf())
}

fn download(req: &Request, part: &Path, sum: Option<&Checksum>) -> io::Result<()> {
    let resp = send(req, None)?;
    let mut reader = resp.into_body().into_reader();
    let mut out = fs::File::create(part)?;
    let mut hasher = sum.map(Hasher::new);
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 { break; }
        if let Some(h) = hasher.as_mut() { h.update(&buf[..n]); }
        out.write_all(&buf[..n])?;
    }
    out.flush()?;
    if let (Some(h), Some(s)) = (hasher, sum)
        && !h.finish_hex().eq_ignore_ascii_case(expected(s)) {
            return Err(io::Error::new(io::ErrorKind::InvalidData,
                format!("checksum mismatch for {}", safe_url(&req.url))));
        }
    Ok(())
}

pub fn get_json(req: &Request) -> io::Result<Value> {
    get_json_within(req, JSON_TIMEOUT)
}

fn get_json_within(req: &Request, limit: Duration) -> io::Result<Value> {
    let resp = send(req, Some(limit))?;
    let mut body = String::new();
    resp.into_body().into_reader().read_to_string(&mut body)?;
    serde_json::from_str(&body).map_err(|_| io::Error::new(io::ErrorKind::InvalidData,
        format!("invalid JSON from {}", safe_url(&req.url))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    const ABC256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("arcus-fetch-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }
    fn req(url: &str) -> Request { Request { url: url.into(), headers: vec![] } }

    fn serve_once(status: &'static str, body: &'static str) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = l.accept() {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let _ = write!(s, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        format!("http://127.0.0.1:{port}/file?token=secret")
    }

    #[test]
    fn digest_known() {
        let d = tmp("digest");
        let f = d.join("a");
        fs::write(&f, "abc").unwrap();
        assert!(file_digest_matches(&f, &Checksum::Sha256(ABC256.into())).unwrap());
        assert!(!file_digest_matches(&f, &Checksum::Sha256("00".into())).unwrap());
        let s512 = "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f";
        assert!(file_digest_matches(&f, &Checksum::Sha512(s512.into())).unwrap());
    }

    #[test]
    fn valid_cache_reused_offline() {
        let d = tmp("valid");
        let f = d.join("a");
        fs::write(&f, "abc").unwrap();
        let r = fetch_cached(&req("http://127.0.0.1:9/x"), &f, Some(&Checksum::Sha256(ABC256.into())));
        assert_eq!(r.unwrap(), f);
    }

    #[test]
    fn corrupt_cache_refetched() {
        let d = tmp("corrupt");
        let f = d.join("a");
        fs::write(&f, "xyz").unwrap();
        let r = fetch_cached(&req("http://127.0.0.1:9/x"), &f, Some(&Checksum::Sha256(ABC256.into())));
        assert!(r.is_err());
        assert!(!f.exists());
    }

    #[test]
    fn mismatch_after_download_leaves_nothing() {
        let d = tmp("mismatch");
        let f = d.join("a");
        let url = serve_once("200 OK", "xyz");
        let e = fetch_cached(&req(&url), &f, Some(&Checksum::Sha256(ABC256.into()))).unwrap_err();
        assert!(!e.to_string().contains("secret"), "{e}");
        assert!(!f.exists());
        assert!(!d.join("a.part").exists());
    }

    #[test]
    fn download_ok_without_sum() {
        let d = tmp("ok");
        let f = d.join("a");
        let url = serve_once("200 OK", "abc");
        assert_eq!(fetch_cached(&req(&url), &f, None).unwrap(), f);
        assert_eq!(fs::read_to_string(&f).unwrap(), "abc");
        assert!(!d.join("a.part").exists());
    }

    #[test]
    fn get_json_times_out_on_silent_server() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            let conn = l.accept();
            let _ = rx.recv_timeout(Duration::from_secs(5)); // hold the connection open, never answer
            drop(conn);
        });
        let started = std::time::Instant::now();
        let e = get_json_within(&req(&format!("http://127.0.0.1:{port}/x?token=secret")), Duration::from_millis(300)).unwrap_err();
        let _ = tx.send(());
        assert!(started.elapsed() < Duration::from_secs(4), "took {:?}", started.elapsed());
        assert!(e.to_string().contains("timeout") && !e.to_string().contains("secret"), "{e}");
    }

    #[test]
    fn get_json_statuses() {
        let url = serve_once("200 OK", "{\"a\":1}");
        assert_eq!(get_json(&req(&url)).unwrap()["a"], 1);
        let e = get_json(&req(&serve_once("404 Not Found", ""))).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
        let e = get_json(&req(&serve_once("403 Forbidden", ""))).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        let e = get_json(&req(&serve_once("401 Unauthorized", ""))).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        let e = get_json(&req(&serve_once("500 Oops", ""))).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::Other);
        assert!(e.to_string().contains("500") && !e.to_string().contains("secret"), "{e}");
    }
}
