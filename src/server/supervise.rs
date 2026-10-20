//! Supervisor for cache server instances. `arcus-cli --supervise <name>` runs detached, owns the
//! memcached child and records how it exited in `last_exit.json` (the hook point for alerting).
use super::home::{Home, InstanceName};
use super::instance::{server_args, InstanceConfig};
use super::jobs::{pid_alive, tail};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const READY_WAIT: Duration = Duration::from_secs(5);
const STOP_WAIT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(100);

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct ExitRecord {
    pub time: String,
    pub code: Option<i32>,
    pub signal: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

pub fn exit_record(status: ExitStatus, time: String) -> ExitRecord {
    ExitRecord { time, code: status.code(), signal: status.signal(), note: None }
}

/// RFC 3339 UTC (`YYYY-MM-DDTHH:MM:SSZ`) for seconds since the Unix epoch.
fn rfc3339(secs: u64) -> String {
    let (days, rem) = ((secs / 86400) as i64, secs % 86400);
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

pub fn now_rfc3339() -> String {
    rfc3339(SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0))
}

struct Paths { dir: PathBuf }
impl Paths {
    fn new(home: &Home, name: &InstanceName) -> Paths { Paths { dir: home.instance(name) } }
    fn server_pid(&self) -> PathBuf { self.dir.join("server.pid") }
    fn last_exit(&self) -> PathBuf { self.dir.join("last_exit.json") }
    fn run_log(&self) -> PathBuf { self.dir.join("run.log") }
    fn config(&self) -> PathBuf { self.dir.join("instance.toml") }
}

pub fn read_pid(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok().and_then(|t| t.trim().parse().ok())
}

fn read_last_exit(p: &Paths) -> Option<ExitRecord> {
    std::fs::read(p.last_exit()).ok().and_then(|b| serde_json::from_slice(&b).ok())
}

fn open_log(p: &Paths) -> io::Result<std::fs::File> {
    OpenOptions::new().create(true).append(true).open(p.run_log())
}

/// Entry point of the detached supervisor process.
pub fn supervise(home: &Home, name: &InstanceName) -> io::Result<()> {
    let p = Paths::new(home, name);
    let cfg = InstanceConfig::load(&p.config())?;
    let prefix = home.build_prefix(&cfg.build_key);
    let log = open_log(&p)?;
    let mut cmd = Command::new(prefix.join("bin/memcached"));
    cmd.args(server_args(&cfg, &prefix)).current_dir(&p.dir);
    run_supervised(&p.dir, cmd, log)
}

/// Runs `cmd` as the child of this supervisor process: `<dir>/supervisor.pid` and `server.pid` while it runs,
/// `last_exit.json` once it has exited (the alert hook point). Shared by cache servers and ZooKeeper nodes.
pub fn run_supervised(dir: &Path, mut cmd: Command, log: std::fs::File) -> io::Result<()> {
    let (server_pid, supervisor_pid, last_exit) = (dir.join("server.pid"), dir.join("supervisor.pid"), dir.join("last_exit.json"));
    let _ = std::fs::remove_file(&last_exit);
    std::fs::write(&supervisor_pid, std::process::id().to_string())?;
    let spawned = cmd.stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log).spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => { let _ = std::fs::remove_file(&supervisor_pid); return Err(e); }
    };
    let _ = std::fs::write(&server_pid, child.id().to_string());
    let waited = child.wait();
    // Alert hook point: the server has exited; `rec` says how.
    let result = waited.and_then(|st| write_exit_record(&last_exit, &exit_record(st, now_rfc3339())));
    let _ = std::fs::remove_file(&server_pid);
    let _ = std::fs::remove_file(&supervisor_pid);
    result
}

pub fn write_exit_record(path: &Path, rec: &ExitRecord) -> io::Result<()> {
    std::fs::write(path, serde_json::to_vec_pretty(rec).map_err(io::Error::other)?)
}

pub fn comm_of(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    { std::fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|s| s.trim().to_string()) }
    #[cfg(not(target_os = "linux"))]
    {
        let out = Command::new("ps").args(["-p", &pid.to_string(), "-o", "comm="]).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

/// True if the pid in `pid_file` is alive and its command name contains `expect_comm`.
pub fn is_running(pid_file: &Path, expect_comm: &str) -> bool {
    let Some(pid) = read_pid(pid_file) else { return false };
    pid_alive(pid) && comm_of(pid).is_some_and(|c| c.contains(expect_comm))
}

/// Sends `version\r\n` and checks for a `VERSION ` line.
fn responds(port: u16, timeout: Duration) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut s) = TcpStream::connect_timeout(&addr, timeout) else { return false };
    let _ = s.set_read_timeout(Some(timeout));
    let _ = s.set_write_timeout(Some(timeout));
    if s.write_all(b"version\r\n").is_err() { return false; }
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).is_ok() && line.starts_with("VERSION ")
}

fn newest_file(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.filter_map(Result::ok)
        .filter(|e| e.path().is_file())
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max().map(|(_, p)| p)
}

fn diagnostics(p: &Paths) -> String {
    let mut out = String::new();
    if let Some(rec) = read_last_exit(p) {
        out.push_str(&format!("\nlast_exit.json: {}", serde_json::to_string(&rec).unwrap_or_default()));
    }
    if let Some(f) = newest_file(&p.dir.join("ARCUSlog")) {
        out.push_str(&format!("\n--- last 20 lines of {} ---\n{}", f.display(), tail(&f, 20).unwrap_or_default()));
    }
    out.push_str(&format!("\n--- last 20 lines of {} ---\n{}", p.run_log().display(), tail(&p.run_log(), 20).unwrap_or_default()));
    out
}

fn load_config(p: &Paths) -> io::Result<InstanceConfig> {
    InstanceConfig::load(&p.config()).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => io::Error::new(io::ErrorKind::NotFound,
            format!("instance not found ({}); run arcus_setup first", p.config().display())),
        _ => e,
    })
}

/// Starts the supervisor (and thereby the server); returns the server pid once it answers `version`.
pub fn start(home: &Home, name: &InstanceName) -> io::Result<u32> {
    let p = Paths::new(home, name);
    let cfg = load_config(&p)?;
    if is_running(&p.server_pid(), "memcached") {
        return Err(io::Error::other(format!("instance `{}` is already running (pid {})",
            name.as_str(), read_pid(&p.server_pid()).unwrap_or(0))));
    }
    if responds(cfg.port, Duration::from_millis(500)) {
        return Err(io::Error::other(format!(
            "something already answers `version` on 127.0.0.1:{}; not starting a second server", cfg.port)));
    }
    let _ = std::fs::remove_file(p.last_exit());
    let log = open_log(&p)?;
    let mut child = Command::new(std::env::current_exe()?)
        .args(["--supervise", name.as_str(), "--home"]).arg(&home.root)
        .process_group(0).stdin(Stdio::null())
        .stdout(log.try_clone()?).stderr(log).spawn()?;
    std::thread::spawn(move || { let _ = child.wait(); });
    let deadline = Instant::now() + READY_WAIT;
    while Instant::now() < deadline {
        if responds(cfg.port, Duration::from_millis(300))
            && let Some(pid) = read_pid(&p.server_pid()) { return Ok(pid); }
        if p.last_exit().exists() { break; }
        std::thread::sleep(POLL);
    }
    let died = p.last_exit().exists() || !is_running(&p.server_pid(), "memcached");
    let pid = read_pid(&p.server_pid());
    Err(io::Error::other(format!("{}{}", start_failure(cfg.port, died, pid), diagnostics(&p))))
}

/// Why `start` gave up: the server exited, or it is alive but did not answer within the wait.
fn start_failure(port: u16, died: bool, pid: Option<u32>) -> String {
    if died {
        format!("server did not answer `version` on port {port} (the server exited)")
    } else {
        format!("server did not answer `version` on port {port} within 5 s, but it is still running{}; \
            it may still be starting. Check again with arcus_status, or stop it with arcus_stop.",
            pid.map(|p| format!(" (pid {p})")).unwrap_or_default())
    }
}

#[derive(Debug, PartialEq, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopOutcome { Stopped, StillRunning, Killed, NotRunning }

fn signal(pid: u32, sig: libc::c_int) {
    if let Ok(p) = libc::pid_t::try_from(pid) {
        // SAFETY: plain kill(2) on a pid we verified to be our memcached.
        unsafe { libc::kill(p, sig); }
    }
}

fn wait_gone(pid: u32, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if !pid_alive(pid) { return true; }
        std::thread::sleep(POLL);
    }
    !pid_alive(pid)
}

pub fn stop(home: &Home, name: &InstanceName, force: bool) -> io::Result<StopOutcome> {
    let p = Paths::new(home, name);
    if !is_running(&p.server_pid(), "memcached") { return Ok(StopOutcome::NotRunning); }
    let Some(pid) = read_pid(&p.server_pid()) else { return Ok(StopOutcome::NotRunning) };
    signal(pid, libc::SIGTERM);
    if wait_gone(pid, STOP_WAIT) { return Ok(StopOutcome::Stopped); }
    if !force { return Ok(StopOutcome::StillRunning); }
    signal(pid, libc::SIGKILL);
    if wait_gone(pid, Duration::from_secs(5)) { Ok(StopOutcome::Killed) }
    else { Err(io::Error::other(format!("pid {pid} survived SIGKILL"))) }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Status {
    pub name: String,
    pub running: bool,
    pub pid: Option<u32>,
    pub port: u16,
    pub responding: bool,
    pub last_exit: Option<ExitRecord>,
}

fn status_of(home: &Home, name: &InstanceName) -> io::Result<Status> {
    let p = Paths::new(home, name);
    let cfg = load_config(&p)?;
    let running = is_running(&p.server_pid(), "memcached");
    Ok(Status {
        name: name.as_str().to_string(), running,
        pid: if running { read_pid(&p.server_pid()) } else { None },
        port: cfg.port,
        responding: responds(cfg.port, Duration::from_millis(500)),
        last_exit: read_last_exit(&p),
    })
}

/// Status of one instance, or of every directory under `instances/` that has an `instance.toml`.
pub fn status(home: &Home, name: Option<&InstanceName>) -> io::Result<Vec<Status>> {
    if let Some(n) = name { return Ok(vec![status_of(home, n)?]); }
    let mut names: Vec<InstanceName> = match std::fs::read_dir(home.root.join("instances")) {
        Ok(rd) => rd.filter_map(Result::ok)
            .filter_map(|e| InstanceName::parse(&e.file_name().to_string_lossy()).ok()).collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => vec![],
        Err(e) => return Err(e),
    };
    names.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    names.iter().filter(|n| Paths::new(home, n).config().exists()).map(|n| status_of(home, n)).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn start_failure_messages() {
        use super::start_failure;
        assert!(start_failure(11211, true, None).contains("the server exited"));
        let m = start_failure(11211, false, Some(42));
        assert!(m.contains("still running") && m.contains("pid 42") && m.contains("arcus_stop") && !m.contains("exited"), "{m}");
    }

    use super::*;

    #[test]
    fn exit_by_signal() {
        assert_eq!(exit_record(ExitStatus::from_raw(9), "t".into()), ExitRecord { time: "t".into(), code: None, signal: Some(9), note: None });
    }

    #[test]
    fn exit_by_code() {
        assert_eq!(exit_record(ExitStatus::from_raw(3 << 8), "t".into()).code, Some(3));
    }

    #[test]
    fn foreign_or_dead_pid_not_running() {
        let dir = std::env::temp_dir().join(format!("arcus-sup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("p.pid");
        std::fs::write(&f, std::process::id().to_string()).unwrap();
        assert!(!is_running(&f, "memcached"));
        assert!(is_running(&f, "arcus"));
        std::fs::write(&f, "999999").unwrap();
        assert!(!is_running(&f, "memcached"));
        assert!(!is_running(&dir.join("missing"), "memcached"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rfc3339_known_values() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_709_210_096), "2024-02-29T12:34:56Z");
        assert_eq!(rfc3339(4_102_444_799), "2099-12-31T23:59:59Z");
    }
}
