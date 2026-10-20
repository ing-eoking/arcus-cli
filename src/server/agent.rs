//! Resident agent (`arcus-cli --agent --home <dir>`), one per machine.
//!
//! It answers one-line JSON requests on `<home>/agent.sock` (mode 600) and starts cache servers and ZooKeeper
//! nodes under their own supervisor processes (`--supervise` / `--supervise-zk`), which are its children.
//! Nothing here ever stops a server or ZooKeeper node because the agent or a supervisor failed:
//! - On Linux the agent is a child subreaper: if a supervisor dies its server is adopted by the agent, which
//!   reaps it and records the exit code/signal in `last_exit.json` (thread B).
//! - A server whose supervisor is gone and whose parent is not the agent (macOS, or the agent was down when
//!   the supervisor died) is watched with pidfd (Linux) / kqueue (macOS); its exit is recorded as
//!   "exit status unknown" (thread C).
//! - A restarted agent finds running supervisors through the pid files and leaves them alone.
use super::home::{Home, InstanceName};
use super::jobs::pid_alive;
use super::supervise::{self, comm_of, now_rfc3339, read_pid, write_exit_record, ExitRecord, Status, StopOutcome};
use super::zookeeper::{self, Node, Started};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TICK: Duration = Duration::from_millis(500);
/// Long enough for the slowest operation (`zk_start` waits up to 20 s per call, stops up to 15 s).
const REPLY_WAIT: Duration = Duration::from_secs(300);
pub const UNKNOWN_EXIT: &str = "exit status unknown";

pub fn sock_path(home: &Home) -> PathBuf { home.root.join("agent.sock") }
fn pid_path(home: &Home) -> PathBuf { home.root.join("agent.pid") }

/// Exit record of a server that was nobody's child when it exited.
pub fn orphan_exit_record(time: String) -> ExitRecord {
    ExitRecord { time, code: None, signal: None, note: Some(UNKNOWN_EXIT.to_string()) }
}

// ---------------------------------------------------------------- client side

/// Sends `req` to the agent if one answers on `agent.sock`; `None` when no agent is running.
pub fn try_request(home: &Home, req: &Value) -> Option<io::Result<Value>> {
    let s = UnixStream::connect(sock_path(home)).ok()?;
    Some(exchange(s, req))
}

fn exchange(s: UnixStream, req: &Value) -> io::Result<Value> {
    s.set_write_timeout(Some(Duration::from_secs(10)))?;
    s.set_read_timeout(Some(REPLY_WAIT))?;
    let mut w = &s;
    w.write_all(format!("{req}\n").as_bytes())?;
    let mut line = String::new();
    BufReader::new(&s).read_line(&mut line)?;
    let reply: Value = serde_json::from_str(&line)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("bad reply from the agent: {e}")))?;
    if reply["ok"] == true { Ok(reply["result"].clone()) }
    else { Err(io::Error::other(reply["error"].as_str().unwrap_or("the agent returned an error").to_string())) }
}

/// One request to the running agent; an error if no agent answers.
pub fn agent_request(home: &Home, req: &Value) -> io::Result<Value> {
    try_request(home, req).unwrap_or_else(|| Err(io::Error::new(io::ErrorKind::NotConnected,
        format!("no agent answers on {}", sock_path(home).display()))))
}

fn decode<T: serde::de::DeserializeOwned>(v: Value) -> io::Result<T> {
    serde_json::from_value(v).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("bad reply from the agent: {e}")))
}

// The functions below are what the tools call: through the agent when it runs, else directly (as before).

pub fn start(home: &Home, name: &InstanceName) -> io::Result<u32> {
    match try_request(home, &json!({"op": "start", "name": name.as_str()})) {
        Some(r) => decode(r?["pid"].take()),
        None => supervise::start(home, name),
    }
}

pub fn stop(home: &Home, name: &InstanceName, force: bool) -> io::Result<StopOutcome> {
    match try_request(home, &json!({"op": "stop", "name": name.as_str(), "force": force})) {
        Some(r) => decode(r?["outcome"].take()),
        None => supervise::stop(home, name, force),
    }
}

pub fn status(home: &Home, name: Option<&InstanceName>) -> io::Result<Vec<Status>> {
    match try_request(home, &json!({"op": "status", "name": name.map(InstanceName::as_str)})) {
        Some(r) => decode(r?["instances"].take()),
        None => supervise::status(home, name),
    }
}

/// Starts the ensemble `members` (may span machines); `my_ids` selects the members that run here.
pub fn zk_start_members(home: &Home, members: &[Node], my_ids: Option<&[u32]>, version: &str) -> io::Result<Started> {
    let req = match my_ids {
        Some(ids) => json!({"op": "zk_start", "members": members, "my_ids": ids, "version": version}),
        None => json!({"op": "zk_start", "nodes": members, "version": version}),
    };
    match try_request(home, &req) {
        Some(r) => decode(r?),
        None => zookeeper::start_members(home, members, my_ids, version),
    }
}

pub fn zk_stop(home: &Home) -> io::Result<usize> {
    match try_request(home, &json!({"op": "zk_stop"})) {
        Some(r) => decode(r?["stopped"].take()),
        None => zookeeper::stop(home),
    }
}

// ---------------------------------------------------------------- what the agent manages

/// A supervised process: a cache server instance or a ZooKeeper node.
struct Target { dir: PathBuf, comm: &'static str }
impl Target {
    fn server_pid(&self) -> Option<u32> { read_pid(&self.dir.join("server.pid")) }
    fn supervisor_pid(&self) -> Option<u32> { read_pid(&self.dir.join("supervisor.pid")) }
    fn server_running(&self) -> Option<u32> {
        self.server_pid().filter(|p| pid_alive(*p) && comm_of(*p).is_some_and(|c| c.contains(self.comm)))
    }
    fn supervisor_running(&self) -> Option<u32> { self.supervisor_pid().filter(|p| pid_alive(*p)) }
    /// Records how the server `pid` exited (if `server.pid` still names it) and drops the pid files.
    fn record_exit(&self, pid: u32, rec: &ExitRecord) {
        if self.server_pid() != Some(pid) { return; }
        // Alert hook point (as in `supervise::run_supervised`): the server has exited; `rec` says how.
        let _ = write_exit_record(&self.dir.join("last_exit.json"), rec);
        let _ = std::fs::remove_file(self.dir.join("server.pid"));
        let _ = std::fs::remove_file(self.dir.join("supervisor.pid"));
    }
}

fn subdirs(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut v: Vec<(String, PathBuf)> = std::fs::read_dir(dir).map(|rd| rd.filter_map(Result::ok)
        .filter(|e| e.path().is_dir()).map(|e| (e.file_name().to_string_lossy().into_owned(), e.path())).collect())
        .unwrap_or_default();
    v.sort();
    v
}

fn instance_targets(home: &Home) -> Vec<(String, Target)> {
    subdirs(&home.root.join("instances")).into_iter()
        .filter(|(n, d)| InstanceName::parse(n).is_ok() && d.join("instance.toml").is_file())
        .map(|(n, dir)| (n, Target { dir, comm: "memcached" })).collect()
}

fn zk_targets(home: &Home) -> Vec<(u32, Target)> {
    let mut v: Vec<(u32, Target)> = subdirs(&home.zk_dir()).into_iter()
        .filter_map(|(n, dir)| Some((n.strip_prefix("node")?.parse().ok()?, Target { dir, comm: "java" }))).collect();
    v.sort_by_key(|(id, _)| *id);
    v
}

fn all_targets(home: &Home) -> Vec<Target> {
    instance_targets(home).into_iter().map(|(_, t)| t).chain(zk_targets(home).into_iter().map(|(_, t)| t)).collect()
}

fn info(home: &Home) -> Value {
    let servers: Vec<Value> = instance_targets(home).into_iter().filter_map(|(name, t)| {
        let (pid, sup) = (t.server_running(), t.supervisor_running());
        (pid.is_some() || sup.is_some()).then(|| json!({"name": name, "pid": pid, "supervisor_pid": sup}))
    }).collect();
    let zk_nodes: Vec<Value> = zk_targets(home).into_iter().filter_map(|(id, t)| {
        let (pid, sup) = (t.server_running(), t.supervisor_running());
        (pid.is_some() || sup.is_some()).then(|| json!({"id": id, "pid": pid, "supervisor_pid": sup}))
    }).collect();
    json!({"tag": crate::TAG, "pid": std::process::id(), "servers": servers, "zk_nodes": zk_nodes})
}

// ---------------------------------------------------------------- server side

struct Agent { home: Home, stop: AtomicBool, ops: Mutex<()> }

fn str_arg(req: &Value, key: &str) -> Result<String, String> {
    req[key].as_str().map(str::to_string).ok_or_else(|| format!("missing string `{key}`"))
}

fn name_arg(req: &Value) -> Result<InstanceName, String> { InstanceName::parse(&str_arg(req, "name")?) }

fn handle(agent: &Agent, req: &Value) -> Result<Value, String> {
    let home = &agent.home;
    let op = req["op"].as_str().unwrap_or("");
    // Starting and stopping is serialized; status and info are not.
    let lock = || agent.ops.lock().unwrap_or_else(|p| p.into_inner());
    let es = |e: io::Error| e.to_string();
    match op {
        "info" => Ok(info(home)),
        "status" => {
            let name = match req["name"].as_str() { Some(n) => Some(InstanceName::parse(n)?), None => None };
            let list = supervise::status(home, name.as_ref()).map_err(es)?;
            Ok(json!({"instances": list}))
        }
        "start" => {
            let name = name_arg(req)?;
            let _g = lock();
            supervise::start(home, &name).map(|pid| json!({"pid": pid})).map_err(es)
        }
        "stop" => {
            let name = name_arg(req)?;
            let _g = lock();
            supervise::stop(home, &name, req["force"].as_bool().unwrap_or(false)).map(|o| json!({"outcome": o})).map_err(es)
        }
        "zk_start" => {
            let (key, my_ids): (&str, Option<Vec<u32>>) = if req["members"].is_null() { ("nodes", None) } else {
                ("members", Some(serde_json::from_value(req["my_ids"].clone()).map_err(|e| format!("bad `my_ids`: {e}"))?))
            };
            let nodes: Vec<Node> = serde_json::from_value(req[key].clone()).map_err(|e| format!("bad `{key}`: {e}"))?;
            let version = str_arg(req, "version")?;
            let _g = lock();
            let st = zookeeper::start_members(home, &nodes, my_ids.as_deref(), &version).map_err(es)?;
            serde_json::to_value(st).map_err(|e| e.to_string())
        }
        "zk_stop" => {
            let _g = lock();
            zookeeper::stop(home).map(|n| json!({"stopped": n})).map_err(es)
        }
        #[cfg(test)]
        "shutdown" => {
            agent.stop.store(true, Ordering::SeqCst);
            Ok(json!({}))
        }
        other => Err(format!("unknown agent op `{other}`")),
    }
}

fn serve_conn(agent: &Agent, s: UnixStream) {
    let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
    let mut line = String::new();
    if BufReader::new(&s).read_line(&mut line).is_err() { return; }
    let reply = match serde_json::from_str::<Value>(&line) {
        Ok(req) => match handle(agent, &req) {
            Ok(v) => json!({"ok": true, "result": v}),
            Err(e) => json!({"ok": false, "error": e}),
        },
        Err(e) => json!({"ok": false, "error": format!("bad request: {e}")}),
    };
    let mut w = &s;
    let _ = w.write_all(format!("{reply}\n").as_bytes());
}

/// Binds `agent.sock` with mode 600 (bound under a temporary name, then renamed into place).
fn bind(home: &Home) -> io::Result<UnixListener> {
    let sock = sock_path(home);
    if std::fs::symlink_metadata(&sock).is_ok() {
        if UnixStream::connect(&sock).is_ok() {
            return Err(io::Error::new(io::ErrorKind::AddrInUse, format!("an agent is already running on {}", sock.display())));
        }
        std::fs::remove_file(&sock)?; // stale: nobody answers
    }
    let tmp = home.root.join(format!("agent.sock.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let l = UnixListener::bind(&tmp)?;
    use std::os::unix::fs::PermissionsExt;
    let placed = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).and_then(|()| std::fs::rename(&tmp, &sock));
    if let Err(e) = placed { let _ = std::fs::remove_file(&tmp); return Err(e); }
    Ok(l)
}

/// Runs the agent in the foreground until it is killed.
pub fn run_agent(home: &Home) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    // SAFETY: prctl with integer arguments only.
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
        eprintln!("WARNING: cannot become a child subreaper: {}", io::Error::last_os_error());
    }
    let listener = bind(home)?;
    std::fs::write(pid_path(home), std::process::id().to_string())?;
    let agent = Arc::new(Agent { home: home.clone(), stop: AtomicBool::new(false), ops: Mutex::new(()) });
    let workers = [
        { let a = agent.clone(); std::thread::spawn(move || reap_loop(&a)) },
        { let a = agent.clone(); std::thread::spawn(move || watch_loop(&a)) },
    ];
    for conn in listener.incoming() {
        if agent.stop.load(Ordering::SeqCst) { break; }
        let Ok(s) = conn else { continue };
        let a = agent.clone();
        std::thread::spawn(move || {
            serve_conn(&a, s);
            // wake the accept loop so it sees the stop flag
            if a.stop.load(Ordering::SeqCst) { let _ = UnixStream::connect(sock_path(&a.home)); }
        });
    }
    agent.stop.store(true, Ordering::SeqCst);
    let _ = std::fs::remove_file(sock_path(home));
    if read_pid(&pid_path(home)) == Some(std::process::id()) { let _ = std::fs::remove_file(pid_path(home)); }
    for w in workers { let _ = w.join(); }
    Ok(())
}

// ---------------------------------------------------------------- thread B: reaping adopted servers

/// Parent pid of `pid` (Linux only; elsewhere the agent never adopts anything).
#[cfg(target_os = "linux")]
fn parent_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // "pid (comm) state ppid ..."; comm may contain spaces, so parse after the last ')'.
    stat[stat.rfind(')')? + 1..].split_whitespace().nth(1)?.parse().ok()
}

/// Reaps servers adopted after their supervisor died. Only pids named by `server.pid` files whose parent is
/// this process are waited for: a `waitpid(-1)` would also steal the children of the agent's own short-lived
/// commands (`ps`, `java -version`) from `Command::output`.
fn reap_loop(agent: &Agent) {
    while !agent.stop.load(Ordering::SeqCst) {
        #[cfg(target_os = "linux")]
        for t in all_targets(&agent.home) {
            let Some(pid) = t.server_pid() else { continue };
            if parent_of(pid) != Some(std::process::id()) || !comm_of(pid).is_some_and(|c| c.contains(t.comm)) { continue; }
            let Ok(raw) = libc::pid_t::try_from(pid) else { continue };
            let mut st = 0;
            // SAFETY: waitpid on a pid that is our (adopted) child.
            if unsafe { libc::waitpid(raw, &mut st, libc::WNOHANG) } == raw {
                use std::os::unix::process::ExitStatusExt;
                t.record_exit(pid, &supervise::exit_record(std::process::ExitStatus::from_raw(st), now_rfc3339()));
            }
        }
        std::thread::sleep(TICK);
    }
}

// ---------------------------------------------------------------- thread C: watching orphans of init/launchd

#[derive(Debug, PartialEq)]
enum Added { Watching, Gone }

/// Only ESRCH means the pid already exited; any other watch error must fall back to polling (never record an exit).
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn add_error_means_gone(errno: Option<i32>) -> bool { errno == Some(libc::ESRCH) }

#[cfg(target_os = "linux")]
mod watcher {
    use super::Added;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::time::Duration;

    #[derive(Default)]
    pub struct Watcher { fds: Vec<(u32, OwnedFd)>, polled: Vec<u32> }
    impl Watcher {
        pub fn new() -> std::io::Result<Watcher> { Ok(Watcher::default()) }
        pub fn add(&mut self, pid: u32) -> Added {
            // SAFETY: pidfd_open(2) with a pid and no flags; returns a new fd or -1.
            let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
            if fd >= 0 {
                // SAFETY: fd is a freshly opened descriptor we own.
                self.fds.push((pid, unsafe { OwnedFd::from_raw_fd(fd as i32) }));
                return Added::Watching;
            }
            if super::add_error_means_gone(std::io::Error::last_os_error().raw_os_error()) { return Added::Gone; }
            self.polled.push(pid); // kernel without pidfd: poll for the pid instead
            Added::Watching
        }
        /// Pids that exited, waiting up to `timeout`.
        pub fn wait(&mut self, timeout: Duration) -> Vec<u32> {
            let mut pfds: Vec<libc::pollfd> = self.fds.iter().map(|(_, fd)| libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 }).collect();
            // SAFETY: pfds is a valid array of pollfd for the duration of the call.
            let n = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, timeout.as_millis() as libc::c_int) };
            let mut gone = Vec::new();
            if n > 0 {
                let ready: Vec<bool> = pfds.iter().map(|p| p.revents != 0).collect();
                let mut i = 0;
                self.fds.retain(|(pid, _)| { let keep = !ready[i]; i += 1; if !keep { gone.push(*pid); } keep });
            }
            self.polled.retain(|p| { let alive = super::pid_alive(*p); if !alive { gone.push(*p); } alive });
            gone
        }
    }
}

#[cfg(target_os = "macos")]
mod watcher {
    use super::Added;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::time::Duration;

    pub struct Watcher { kq: OwnedFd, polled: Vec<u32> }
    impl Watcher {
        pub fn new() -> std::io::Result<Watcher> {
            // SAFETY: kqueue(2) takes no arguments; returns a new fd or -1.
            let fd = unsafe { libc::kqueue() };
            if fd < 0 { return Err(std::io::Error::last_os_error()); }
            // SAFETY: fd is a freshly opened descriptor we own.
            Ok(Watcher { kq: unsafe { OwnedFd::from_raw_fd(fd) }, polled: Vec::new() })
        }
        pub fn add(&mut self, pid: u32) -> Added {
            let ev = libc::kevent { ident: pid as libc::uintptr_t, filter: libc::EVFILT_PROC, flags: libc::EV_ADD | libc::EV_ONESHOT,
                fflags: libc::NOTE_EXIT, data: 0, udata: std::ptr::null_mut() };
            // SAFETY: one valid changelist entry, no eventlist.
            let r = unsafe { libc::kevent(self.kq.as_raw_fd(), &ev, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
            if r >= 0 { return Added::Watching; }
            if super::add_error_means_gone(std::io::Error::last_os_error().raw_os_error()) { return Added::Gone; }
            self.polled.push(pid); // any other kevent error: poll for the pid instead
            Added::Watching
        }
        pub fn wait(&mut self, timeout: Duration) -> Vec<u32> {
            // SAFETY: kevent is plain data; zeroed is a valid value.
            let mut evs: [libc::kevent; 16] = unsafe { std::mem::zeroed() };
            let ts = libc::timespec { tv_sec: timeout.as_secs() as libc::time_t, tv_nsec: timeout.subsec_nanos() as libc::c_long };
            // SAFETY: evs has room for 16 events; ts is valid for the call.
            let n = unsafe { libc::kevent(self.kq.as_raw_fd(), std::ptr::null(), 0, evs.as_mut_ptr(), 16, &ts) };
            let mut gone: Vec<u32> = evs[..n.max(0) as usize].iter().filter(|e| e.filter == libc::EVFILT_PROC).map(|e| e.ident as u32).collect();
            self.polled.retain(|p| { let alive = super::pid_alive(*p); if !alive { gone.push(*p); } alive });
            gone
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod watcher {
    use super::Added;
    use std::time::Duration;

    pub struct Watcher { polled: Vec<u32> }
    impl Watcher {
        pub fn new() -> std::io::Result<Watcher> { Ok(Watcher { polled: Vec::new() }) }
        pub fn add(&mut self, pid: u32) -> Added { self.polled.push(pid); Added::Watching }
        pub fn wait(&mut self, timeout: Duration) -> Vec<u32> {
            std::thread::sleep(timeout);
            let mut gone = Vec::new();
            self.polled.retain(|p| { let alive = super::pid_alive(*p); if !alive { gone.push(*p); } alive });
            gone
        }
    }
}

/// True if nobody will `waitpid` the running server of `t`: its supervisor is gone and (on Linux) the
/// agent did not adopt it.
fn orphaned(t: &Target) -> Option<u32> {
    let pid = t.server_pid().filter(|p| pid_alive(*p))?;
    if t.supervisor_running().is_some() { return None; }
    #[cfg(target_os = "linux")]
    if parent_of(pid) == Some(std::process::id()) { return None; }
    comm_of(pid).is_some_and(|c| c.contains(t.comm)).then_some(pid)
}

fn watch_loop(agent: &Agent) {
    let mut w = match watcher::Watcher::new() {
        Ok(w) => w,
        Err(e) => { eprintln!("WARNING: cannot watch orphaned servers: {e}"); return; }
    };
    let mut watched: HashMap<u32, Target> = HashMap::new();
    while !agent.stop.load(Ordering::SeqCst) {
        for t in all_targets(&agent.home) {
            let Some(pid) = orphaned(&t) else { continue };
            if watched.contains_key(&pid) { continue; }
            match w.add(pid) {
                Added::Watching => { watched.insert(pid, t); }
                Added::Gone => t.record_exit(pid, &orphan_exit_record(now_rfc3339())),
            }
        }
        if watched.is_empty() { std::thread::sleep(TICK); continue; }
        for pid in w.wait(TICK) {
            if let Some(t) = watched.remove(&pid) { t.record_exit(pid, &orphan_exit_record(now_rfc3339())); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_esrch_means_already_exited() {
        assert!(add_error_means_gone(Some(libc::ESRCH)));
        assert!(!add_error_means_gone(Some(libc::EACCES)));
        assert!(!add_error_means_gone(Some(libc::ENOMEM)));
        assert!(!add_error_means_gone(None));
    }
    use std::time::{Duration, Instant};

    fn tmp_home(tag: &str) -> Home {
        // short path: Unix socket paths are limited to ~104 bytes
        let root = std::path::PathBuf::from(format!("/tmp/arcus-ag-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        Home::new(Some(root)).unwrap()
    }

    fn wait_info(home: &Home) -> Value {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match agent_request(home, &json!({"op": "info"})) {
                Ok(v) => return v,
                Err(e) if Instant::now() > deadline => panic!("agent did not answer: {e}"),
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }

    fn run_and_check(home: &Home) {
        let h = home.clone();
        let t = std::thread::spawn(move || run_agent(&h));
        let info = wait_info(home);
        assert_eq!(info["pid"], json!(std::process::id()));
        assert_eq!(info["servers"], json!([]));
        assert_eq!(info["zk_nodes"], json!([]));
        assert_eq!(info["tag"], json!(crate::TAG));
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(home.root.join("agent.sock")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(std::fs::read_to_string(home.root.join("agent.pid")).unwrap().trim(), std::process::id().to_string());
        agent_request(home, &json!({"op": "shutdown"})).unwrap();
        t.join().unwrap().unwrap();
        assert!(agent_request(home, &json!({"op": "info"})).is_err());
    }

    #[test]
    fn agent_protocol_info_roundtrip() {
        let home = tmp_home("info");
        run_and_check(&home);
        let e = agent_request(&home, &json!({"op": "info"})).unwrap_err();
        assert!(e.to_string().contains("agent"), "{e}");
        let _ = std::fs::remove_dir_all(&home.root);
    }

    #[test]
    fn unknown_op_is_an_error_reply() {
        let home = tmp_home("unk");
        let h = home.clone();
        let t = std::thread::spawn(move || run_agent(&h));
        wait_info(&home);
        let e = agent_request(&home, &json!({"op": "bogus"})).unwrap_err();
        assert!(e.to_string().contains("bogus"), "{e}");
        let e = agent_request(&home, &json!({"op": "start", "name": "../x"})).unwrap_err();
        assert!(e.to_string().contains("invalid instance name"), "{e}");
        agent_request(&home, &json!({"op": "shutdown"})).unwrap();
        t.join().unwrap().unwrap();
        let _ = std::fs::remove_dir_all(&home.root);
    }

    #[test]
    fn orphan_exit_record_has_unknown_note() {
        let r = orphan_exit_record("t".into());
        assert_eq!((r.code, r.signal, r.note.as_deref()), (None, None, Some("exit status unknown")));
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(j, json!({"time": "t", "code": null, "signal": null, "note": "exit status unknown"}));
        // records written before `note` existed still parse
        let old: ExitRecord = serde_json::from_value(json!({"time": "t", "code": 0, "signal": null})).unwrap();
        assert_eq!(old.note, None);
    }

    #[test]
    fn stale_socket_is_replaced() {
        let home = tmp_home("stale");
        let sock = home.root.join("agent.sock");
        drop(std::os::unix::net::UnixListener::bind(&sock).unwrap()); // leaves a dead socket file
        assert!(sock.exists());
        run_and_check(&home);
        let _ = std::fs::remove_dir_all(&home.root);
    }

    #[test]
    fn second_agent_is_refused() {
        let home = tmp_home("dup");
        let h = home.clone();
        let t = std::thread::spawn(move || run_agent(&h));
        wait_info(&home);
        let e = run_agent(&home).unwrap_err();
        assert!(e.to_string().contains("already running"), "{e}");
        agent_request(&home, &json!({"op": "shutdown"})).unwrap();
        t.join().unwrap().unwrap();
        let _ = std::fs::remove_dir_all(&home.root);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn subreaper_adopts_grandchild() {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};
        // SAFETY: prctl with integer arguments only.
        assert_eq!(unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) }, 0);
        let mut sh = Command::new("sh").args(["-c", "sleep 5 & echo $!; exit 0"]).stdout(Stdio::piped()).spawn().unwrap();
        let mut line = String::new();
        BufReader::new(sh.stdout.take().unwrap()).read_line(&mut line).unwrap();
        let pid: libc::pid_t = line.trim().parse().unwrap();
        sh.wait().unwrap();
        // SAFETY: plain kill/waitpid on the adopted grandchild.
        unsafe { libc::kill(pid, libc::SIGKILL); }
        let mut st = 0;
        let got = unsafe { libc::waitpid(pid, &mut st, 0) };
        unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 0, 0, 0, 0); }
        assert_eq!(got, pid);
        assert!(libc::WIFSIGNALED(st) && libc::WTERMSIG(st) == libc::SIGKILL);
    }
}
