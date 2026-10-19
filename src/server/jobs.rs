//! Detached background jobs. State lives in `<home>/jobs/<id>/{spec,status}.json` + `job.log`.

use super::home::Home;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::OpenOptions;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Install,
    ZkDownload,
    EeLogin,
    #[cfg(test)]
    Test,
}

impl JobKind {
    fn name(self) -> String {
        serde_json::to_value(self).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobState { Running, Succeeded, Failed }

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct JobSpec { pub kind: JobKind, pub params: Value }

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct JobStatus {
    pub id: String,
    pub kind: JobKind,
    pub state: JobState,
    pub step: String,
    pub message: String,
    pub log: PathBuf,
    pub result: Option<Value>,
    /// Pid of the detached job process (set by the job itself once it starts).
    #[serde(default)]
    pub pid: Option<u32>,
}

pub struct JobCtx { pub id: String, pub home: Home, pub log: PathBuf }

impl JobCtx {
    /// Records the current step in `status.json` (best effort).
    pub fn step(&self, name: &str) {
        if let Ok(mut s) = read_status(&self.home, &self.id) {
            s.step = name.to_string();
            let _ = write_status(&self.home, &s);
        }
    }
}

/// A `running` job with no known pid is considered dead once `status.json` is this old.
const NO_PID_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

type Handler = fn(&JobCtx, &Value) -> Result<Value, String>;

/// Job handler lookup; Tasks 6-8 register Install / ZkDownload / EeLogin here.
fn handler(kind: JobKind) -> Option<Handler> {
    match kind {
        #[cfg(test)]
        JobKind::Test => Some(test_handler),
        _ => None,
    }
}

#[cfg(test)]
fn test_handler(ctx: &JobCtx, params: &Value) -> Result<Value, String> {
    ctx.step("working");
    match params.get("mode").and_then(Value::as_str) {
        Some("ok") => Ok(serde_json::json!({"x": 1})),
        Some("fail") => {
            let text: String = (1..=30).map(|i| format!("line {i}\n")).collect();
            std::fs::write(&ctx.log, text).map_err(|e| e.to_string())?;
            Err("boom".into())
        }
        Some("panic") => panic!("kaboom"),
        _ => Ok(Value::Null),
    }
}

/// Job ids are used as path components: restrict them.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 96 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn job_dir(home: &Home, id: &str) -> io::Result<PathBuf> {
    if !valid_id(id) { return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("invalid job id `{id}`"))); }
    Ok(home.jobs().join(id))
}

fn read_status(home: &Home, id: &str) -> io::Result<JobStatus> {
    let p = job_dir(home, id)?.join("status.json");
    let text = std::fs::read_to_string(&p).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => io::Error::new(io::ErrorKind::NotFound, format!("job `{id}` not found")),
        _ => e,
    })?;
    serde_json::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Atomic write (temp file + rename) so pollers never see a partial file.
fn write_status(home: &Home, s: &JobStatus) -> io::Result<()> {
    let dir = job_dir(home, &s.id)?;
    let tmp = dir.join("status.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(s).map_err(io::Error::other)?)?;
    std::fs::rename(&tmp, dir.join("status.json"))
}

/// Creates the job directory with `spec.json` and an initial `running` status; returns the id.
fn prepare(home: &Home, spec: &JobSpec) -> io::Result<String> {
    let millis = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis()).unwrap_or(0);
    let mut id = format!("{}-{millis}", spec.kind.name());
    let mut n = 1;
    while job_dir(home, &id)?.exists() { id = format!("{}-{millis}-{n}", spec.kind.name()); n += 1; }
    let dir = job_dir(home, &id)?;
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("spec.json"), serde_json::to_vec_pretty(spec).map_err(io::Error::other)?)?;
    write_status(home, &JobStatus {
        id: id.clone(), kind: spec.kind, state: JobState::Running, step: "queued".into(),
        message: String::new(), log: dir.join("job.log"), result: None, pid: None,
    })?;
    Ok(id)
}

/// Starts a detached `arcus-cli --job <id>` process (own process group, output to `job.log`).
/// Nothing is written to this process's stdout.
pub fn submit(home: &Home, spec: JobSpec) -> io::Result<String> {
    let id = prepare(home, &spec)?;
    let log = OpenOptions::new().create(true).append(true).open(job_dir(home, &id)?.join("job.log"))?;
    let spawned = Command::new(std::env::current_exe()?)
        .args(["--job", &id, "--home"]).arg(&home.root)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn();
    match spawned {
        // Reap the child when it ends so a crashed job is not mistaken for a live zombie.
        Ok(mut child) => {
            // Written by the parent right after spawn, so it exists even if the child dies early.
            let _ = std::fs::write(job_dir(home, &id)?.join("pid"), child.id().to_string());
            std::thread::spawn(move || { let _ = child.wait(); });
            Ok(id)
        }
        Err(e) => {
            if let Ok(mut s) = read_status(home, &id) {
                s.state = JobState::Failed;
                s.message = format!("failed to start job process: {e}");
                let _ = write_status(home, &s);
            }
            Err(e)
        }
    }
}

fn finish(home: &Home, id: &str, outcome: Result<Value, String>) -> io::Result<()> {
    let mut s = read_status(home, id)?;
    match outcome {
        Ok(v) => { s.state = JobState::Succeeded; s.result = Some(v); }
        Err(msg) => {
            let t = tail(&s.log, 20).unwrap_or_default();
            s.state = JobState::Failed;
            s.message = if t.is_empty() { msg } else { format!("{msg}\n{t}") };
        }
    }
    write_status(home, &s)
}

/// Entry point of the detached process: runs the handler and records the final state.
pub fn run_job(home: &Home, id: &str) -> io::Result<()> {
    let mut s = read_status(home, id)?;
    s.pid = Some(std::process::id());
    write_status(home, &s)?;
    let spec: JobSpec = serde_json::from_slice(&std::fs::read(job_dir(home, id)?.join("spec.json"))?)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let outcome = match handler(spec.kind) {
        None => Err(format!("no handler for {}", spec.kind.name())),
        Some(h) => {
            let ctx = JobCtx { id: id.to_string(), home: home.clone(), log: s.log.clone() };
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| h(&ctx, &spec.params))) {
                Ok(r) => r,
                Err(p) => {
                    let m = p.downcast_ref::<&str>().map(|s| s.to_string())
                        .or_else(|| p.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown panic".into());
                    Err(format!("job panicked: {m}"))
                }
            }
        }
    };
    finish(home, id, outcome)
}

fn pid_alive(pid: u32) -> bool {
    let Ok(p) = libc::pid_t::try_from(pid) else { return false };
    // SAFETY: signal 0 only checks for existence/permission.
    unsafe { libc::kill(p, 0) == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

/// Current status; a `running` job whose process is gone is reported as `failed`.
/// Unknown id -> `ErrorKind::NotFound`.
pub fn status(home: &Home, id: &str) -> io::Result<JobStatus> {
    let mut s = read_status(home, id)?;
    if s.state == JobState::Running {
        let dir = job_dir(home, id)?;
        let pid = s.pid.or_else(|| {
            std::fs::read_to_string(dir.join("pid")).ok().and_then(|t| t.trim().parse().ok())
        });
        let gone = match pid {
            Some(pid) => !pid_alive(pid),
            // No pid from either source: the job never started (or died before recording it).
            None => std::fs::metadata(dir.join("status.json")).and_then(|m| m.modified()).ok()
                .and_then(|t| t.elapsed().ok()).is_some_and(|age| age > NO_PID_GRACE),
        };
        if gone {
            // The job may have finished between the read and the liveness check.
            let again = read_status(home, id)?;
            if again.state != JobState::Running { return Ok(again); }
            s.state = JobState::Failed;
            let t = tail(&s.log, 20).unwrap_or_default();
            s.message = if t.is_empty() { "job process exited unexpectedly".into() }
                else { format!("job process exited unexpectedly\n{t}") };
        }
    }
    Ok(s)
}

/// Last `n` lines of the file joined with `\n`. A missing file yields an empty string.
pub fn tail(path: &Path, n: usize) -> io::Result<String> {
    let text = match std::fs::read(path) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => return Err(e),
    };
    let lines: Vec<&str> = text.lines().collect();
    Ok(lines[lines.len().saturating_sub(n)..].join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn home(tag: &str) -> Home {
        let d = std::env::temp_dir().join(format!("arcus-cli-jobs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        Home::new(Some(d)).unwrap()
    }

    #[test] fn job_success_records_result() {
        let h = home("ok");
        let id = prepare(&h, &JobSpec { kind: JobKind::Test, params: json!({"mode": "ok"}) }).unwrap();
        run_job(&h, &id).unwrap();
        let s = status(&h, &id).unwrap();
        assert_eq!(s.state, JobState::Succeeded);
        assert_eq!(s.result, Some(json!({"x": 1})));
        assert_eq!(s.step, "working");
    }
    #[test] fn job_failure_records_tail() {
        let h = home("fail");
        let id = prepare(&h, &JobSpec { kind: JobKind::Test, params: json!({"mode": "fail"}) }).unwrap();
        run_job(&h, &id).unwrap();
        let s = status(&h, &id).unwrap();
        assert_eq!(s.state, JobState::Failed);
        assert!(s.message.contains("boom") && s.message.contains("line 30"));
        assert!(!s.message.contains("line 5\n") && !s.message.ends_with("line 5"));
    }
    #[test] fn job_panic_is_failed() {
        let h = home("panic");
        let id = prepare(&h, &JobSpec { kind: JobKind::Test, params: json!({"mode": "panic"}) }).unwrap();
        run_job(&h, &id).unwrap();
        let s = status(&h, &id).unwrap();
        assert_eq!(s.state, JobState::Failed);
        assert!(s.message.contains("kaboom"));
    }
    #[test] fn unregistered_kind_fails() {
        let h = home("nohandler");
        let id = prepare(&h, &JobSpec { kind: JobKind::EeLogin, params: json!({}) }).unwrap();
        run_job(&h, &id).unwrap();
        let s = status(&h, &id).unwrap();
        assert_eq!(s.state, JobState::Failed);
        assert!(s.message.contains("no handler for ee_login"));
    }
    #[test] fn dead_running_job_is_failed() {
        let h = home("dead");
        let id = prepare(&h, &JobSpec { kind: JobKind::Test, params: json!({}) }).unwrap();
        let mut s = status(&h, &id).unwrap();
        assert_eq!(s.state, JobState::Running);
        s.pid = Some(999_999);
        write_status(&h, &s).unwrap();
        let s = status(&h, &id).unwrap();
        assert_eq!(s.state, JobState::Failed);
        assert!(s.message.contains("job process exited unexpectedly"));
    }
    #[test] fn dead_pid_file_without_status_pid_is_failed() {
        let h = home("pidfile");
        let id = prepare(&h, &JobSpec { kind: JobKind::Test, params: json!({}) }).unwrap();
        let pid_file = h.jobs().join(&id).join("pid");
        std::fs::write(&pid_file, "999999").unwrap();
        let s = status(&h, &id).unwrap();
        assert_eq!(s.state, JobState::Failed);
        assert!(s.message.contains("job process exited unexpectedly"));
        std::fs::write(&pid_file, std::process::id().to_string()).unwrap();
        assert_eq!(status(&h, &id).unwrap().state, JobState::Running);
    }
    #[test] fn no_pid_and_old_status_is_failed() {
        let h = home("nopid");
        let id = prepare(&h, &JobSpec { kind: JobKind::Test, params: json!({}) }).unwrap();
        assert_eq!(status(&h, &id).unwrap().state, JobState::Running); // fresh: still starting
        let f = std::fs::OpenOptions::new().write(true).open(h.jobs().join(&id).join("status.json")).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(60)).unwrap();
        let s = status(&h, &id).unwrap();
        assert_eq!(s.state, JobState::Failed);
        assert!(s.message.contains("job process exited unexpectedly"));
    }
    #[test] fn live_running_job_stays_running() {
        let h = home("live");
        let id = prepare(&h, &JobSpec { kind: JobKind::Test, params: json!({}) }).unwrap();
        let mut s = status(&h, &id).unwrap();
        s.pid = Some(std::process::id());
        write_status(&h, &s).unwrap();
        assert_eq!(status(&h, &id).unwrap().state, JobState::Running);
    }
    #[test] fn unknown_job_is_not_found() {
        let h = home("nf");
        assert_eq!(status(&h, "install-1").unwrap_err().kind(), io::ErrorKind::NotFound);
        assert!(status(&h, "../x").is_err());
    }
    #[test] fn tail_last_lines() {
        let h = home("tail");
        let p = h.root.join("t.log");
        std::fs::write(&p, "a\nb\nc\nd\n").unwrap();
        assert_eq!(tail(&p, 2).unwrap(), "c\nd");
        assert_eq!(tail(&p, 10).unwrap(), "a\nb\nc\nd");
        assert_eq!(tail(&h.root.join("missing"), 3).unwrap(), "");
    }
    #[test] fn kind_serializes_snake_case() {
        assert_eq!(serde_json::to_value(JobKind::ZkDownload).unwrap(), json!("zk_download"));
        assert_eq!(serde_json::to_value(JobState::Succeeded).unwrap(), json!("succeeded"));
    }
}
