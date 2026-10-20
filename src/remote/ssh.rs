//! The single SSH/scp runner used by every remote feature: ControlMaster reuse,
//! GUI password dialog through `SSH_ASKPASS` (the password never reaches the AI or
//! the MCP stdout), host key handling and error classification.

use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use crate::remote::machines::{Machine, Target};
use crate::server::home::Home;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HostKeyPolicy { Strict, AcceptNew }

impl HostKeyPolicy {
    fn option(self) -> &'static str {
        match self {
            HostKeyPolicy::Strict => "StrictHostKeyChecking=yes",
            HostKeyPolicy::AcceptNew => "StrictHostKeyChecking=accept-new",
        }
    }
}

#[derive(Debug)]
pub struct SshOutput { pub status: i32, pub stdout: Vec<u8>, pub stderr: String }

pub trait Runner {
    fn run(&self, program: &str, args: &[String], stdin: &[u8], env: &[(String, String)]) -> io::Result<SshOutput>;
}

pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, program: &str, args: &[String], stdin: &[u8], env: &[(String, String)]) -> io::Result<SshOutput> {
        let mut cmd = Command::new(program);
        cmd.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        for (k, v) in env { cmd.env(k, v); }
        let mut child = cmd.spawn()?;
        let mut pipe = child.stdin.take();
        let data = stdin.to_vec();
        let writer = std::thread::spawn(move || {
            if let Some(p) = pipe.as_mut() { let _ = p.write_all(&data); }
        });
        let out = child.wait_with_output()?;
        let _ = writer.join();
        Ok(SshOutput {
            status: out.status.code().unwrap_or(-1),
            stdout: out.stdout,
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

/// Options added on top of the user's ssh config (creates the control dir, mode 700).
pub fn common_opts(home: &Home) -> Vec<String> {
    let dir = home.ssh_dir();
    let _ = std::fs::create_dir_all(&dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let mut v = Vec::new();
    for o in [
        "ControlMaster=auto".to_string(),
        format!("ControlPath={}/%C", dir.display()),
        "ControlPersist=10m".to_string(),
        "ConnectTimeout=15".to_string(),
    ] {
        v.push("-o".to_string());
        v.push(o);
    }
    v
}

pub fn gui_available() -> bool {
    if cfg!(target_os = "macos") {
        std::env::var_os("SSH_CONNECTION").is_none()
    } else {
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
    }
}

/// Environment for the ssh child. With a GUI, ssh runs this binary as its askpass program.
pub fn askpass_env(gui: bool) -> Vec<(String, String)> {
    if !gui {
        return vec![("SSH_ASKPASS_REQUIRE".into(), "never".into())];
    }
    let exe = std::env::current_exe().ok().and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| "arcus-cli".to_string());
    let mut env = vec![
        ("SSH_ASKPASS".to_string(), exe),
        ("SSH_ASKPASS_REQUIRE".to_string(), "force".to_string()),
        ("ARCUS_ASKPASS".to_string(), "1".to_string()),
    ];
    if cfg!(target_os = "macos") && std::env::var_os("DISPLAY").is_none() {
        env.push(("DISPLAY".into(), ":0".into()));
    }
    env
}

/// ssh's askpass protocol: print the answer on this (separate) process's stdout.
/// Returns the exit code. Never run inside `--mcp`.
pub fn askpass_main(prompt: &str) -> i32 {
    /* Host keys are handled through accept_host_key, never through a dialog. */
    if prompt.contains("yes/no") {
        println!("no");
        return 0;
    }
    let out = if cfg!(target_os = "macos") {
        Command::new("osascript")
            .args(["-e", "on run argv",
                   "-e", "text returned of (display dialog (item 1 of argv) default answer \"\" with hidden answer)",
                   "-e", "end run", prompt])
            .stdin(Stdio::null()).stderr(Stdio::null()).output()
    } else {
        Command::new("zenity").args(["--password", "--title", prompt])
            .stdin(Stdio::null()).stderr(Stdio::null()).output()
    };
    match out {
        Ok(o) if o.status.success() => {
            let _ = io::stdout().write_all(&o.stdout);
            0
        }
        _ => 1,
    }
}

#[derive(Debug)]
pub enum SshError {
    HostKeyUnknown { fingerprint: String },
    HostKeyChanged,
    AuthFailed { hint: String },
    Unreachable(String),
    Remote { status: i32, stderr: String },
}

pub fn classify(stderr: &str, status: i32, dest_label: &str) -> Option<SshError> {
    if stderr.contains("REMOTE HOST IDENTIFICATION HAS CHANGED") {
        return Some(SshError::HostKeyChanged);
    }
    if stderr.contains("Host key verification failed") || stderr.contains("host key is known") {
        return Some(SshError::HostKeyUnknown { fingerprint: String::new() });
    }
    if stderr.contains("Permission denied (") {
        return Some(SshError::AuthFailed {
            hint: format!("run \"ssh-copy-id {dest_label}\" once in a terminal"),
        });
    }
    if stderr.contains("Could not resolve hostname") || stderr.contains("Connection refused")
        || stderr.contains("timed out") {
        return Some(SshError::Unreachable(stderr.trim().to_string()));
    }
    let _ = status;
    None
}

pub fn fingerprint(runner: &dyn Runner, host: &str, port: u16) -> io::Result<String> {
    let scan = runner.run("ssh-keyscan", &["-p".into(), port.to_string(), host.into()], &[], &[])?;
    let keys: Vec<u8> = scan.stdout.split(|&b| b == b'\n')
        .filter(|l| !l.is_empty() && l[0] != b'#').flat_map(|l| l.iter().chain(b"\n")).copied().collect();
    if keys.is_empty() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("no host key from {host}:{port}")));
    }
    let out = runner.run("ssh-keygen", &["-lf".into(), "-".into()], &keys, &[])?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().next().map(|l| l.trim().to_string()).filter(|l| !l.is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "ssh-keygen returned nothing"))
}

pub struct Ssh<'a> {
    pub home: &'a Home,
    pub runner: &'a dyn Runner,
    pub machine: &'a Machine,
    pub policy: HostKeyPolicy,
}

impl Ssh<'_> {
    fn label(&self) -> String {
        match &self.machine.target {
            Target::SshAlias => self.machine.name.clone(),
            Target::Explicit { host, user, .. } => format!("{user}@{host}"),
        }
    }

    fn base_opts(&self, gui: bool) -> Vec<String> {
        let mut v = common_opts(self.home);
        v.push("-o".into());
        v.push(self.policy.option().into());
        if !gui {
            v.push("-o".into());
            v.push("BatchMode=yes".into());
        }
        v
    }

    fn ssh_argv(&self, remote_cmd: &str, gui: bool) -> Vec<String> {
        let mut a = self.base_opts(gui);
        a.extend(self.machine.ssh_destination());
        a.push(remote_cmd.to_string());
        a
    }

    fn scp_argv(&self, local: &Path, remote: &str, gui: bool) -> Vec<String> {
        let mut a = self.base_opts(gui);
        let dest = match &self.machine.target {
            Target::SshAlias => format!("{}:{}", self.machine.name, remote),
            Target::Explicit { host, user, port, identity_file } => {
                a.push("-P".into());
                a.push(port.to_string());
                if let Some(f) = identity_file { a.push("-i".into()); a.push(f.clone()); }
                format!("{user}@{host}:{remote}")
            }
        };
        a.push("--".into());
        a.push(local.display().to_string());
        a.push(dest);
        a
    }

    /// Where `ssh -G` says this machine really connects (for host key lookups).
    fn resolve_endpoint(&self) -> Option<(String, u16)> {
        let mut args = vec!["-G".to_string()];
        args.extend(self.machine.ssh_destination());
        let out = self.runner.run("ssh", &args, &[], &[]).ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let (mut host, mut port) = (None, 22u16);
        for line in text.lines() {
            if let Some(h) = line.strip_prefix("hostname ") { host = Some(h.trim().to_string()); }
            if let Some(p) = line.strip_prefix("port ") { port = p.trim().parse().unwrap_or(22); }
        }
        host.map(|h| (h, port))
    }

    fn finish(&self, out: SshOutput) -> Result<SshOutput, SshError> {
        /* 255 is ssh's own failure; any other status is the remote command's own. */
        if out.status != 255 { return Ok(out); }
        match classify(&out.stderr, out.status, &self.label()) {
            Some(SshError::HostKeyUnknown { .. }) => {
                let fp = self.resolve_endpoint()
                    .and_then(|(h, p)| fingerprint(self.runner, &h, p).ok())
                    .unwrap_or_else(|| "unavailable".to_string());
                Err(SshError::HostKeyUnknown { fingerprint: fp })
            }
            Some(e) => Err(e),
            None => Err(SshError::Remote { status: out.status, stderr: out.stderr }),
        }
    }

    /// Run `remote_cmd` (one argument, interpreted by the remote shell).
    pub fn exec(&self, remote_cmd: &str, stdin: &[u8]) -> Result<SshOutput, SshError> {
        let gui = gui_available();
        let out = self.runner.run("ssh", &self.ssh_argv(remote_cmd, gui), stdin, &askpass_env(gui))
            .map_err(|e| SshError::Unreachable(e.to_string()))?;
        self.finish(out)
    }

    pub fn copy(&self, local: &Path, remote: &str) -> Result<(), SshError> {
        let gui = gui_available();
        let out = self.runner.run("scp", &self.scp_argv(local, remote, gui), &[], &askpass_env(gui))
            .map_err(|e| SshError::Unreachable(e.to_string()))?;
        if out.status == 0 { return Ok(()); }
        if out.status != 255 {
            return Err(SshError::Remote { status: out.status, stderr: out.stderr });
        }
        self.finish(out).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Fake { calls: RefCell<Vec<(String, Vec<String>, Vec<(String, String)>)>>, reply: RefCell<Vec<SshOutput>> }
    impl Fake {
        fn new(reply: Vec<SshOutput>) -> Fake { Fake { calls: RefCell::new(vec![]), reply: RefCell::new(reply) } }
    }
    impl Runner for Fake {
        fn run(&self, p: &str, a: &[String], _i: &[u8], e: &[(String, String)]) -> io::Result<SshOutput> {
            self.calls.borrow_mut().push((p.into(), a.to_vec(), e.to_vec()));
            let mut r = self.reply.borrow_mut();
            Ok(if r.is_empty() { SshOutput { status: 0, stdout: vec![], stderr: String::new() } } else { r.remove(0) })
        }
    }

    fn tmp_home(tag: &str) -> Home {
        let d = std::env::temp_dir().join(format!("arcus-ssh-test-{}-{tag}", std::process::id()));
        Home { root: d }
    }
    fn alias(n: &str) -> Machine { Machine { name: n.into(), target: Target::SshAlias } }
    fn explicit() -> Machine {
        Machine { name: "m".into(), target: Target::Explicit {
            host: "h.example".into(), user: "u".into(), port: 2222, identity_file: Some("/k".into()) } }
    }

    #[test]
    fn exec_argv_for_alias() {
        let home = tmp_home("exec");
        let fake = Fake::new(vec![]);
        let m = alias("dev-1");
        let ssh = Ssh { home: &home, runner: &fake, machine: &m, policy: HostKeyPolicy::Strict };
        ssh.exec("uname -a", b"").unwrap();
        let calls = fake.calls.borrow();
        let (prog, argv, _) = &calls[0];
        assert_eq!(prog, "ssh");
        let common = common_opts(&home);
        assert!(argv.starts_with(&common));
        assert_eq!(&argv[common.len()..common.len() + 2], ["-o", "StrictHostKeyChecking=yes"]);
        assert_eq!(&argv[argv.len() - 2..], ["dev-1", "uname -a"]);
        assert!(common.contains(&"ControlPersist=10m".to_string()));
        let _ = std::fs::remove_dir_all(&home.root);
    }

    #[test]
    fn classify_errors() {
        assert!(matches!(classify("Host key verification failed.", 255, "dev-1"), Some(SshError::HostKeyUnknown{..})));
        assert!(matches!(classify("@@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @@@\nHost key verification failed.", 255, "d"), Some(SshError::HostKeyChanged)));
        match classify("u@h: Permission denied (publickey,password).", 255, "u@h") {
            Some(SshError::AuthFailed{hint}) => assert!(hint.contains("ssh-copy-id u@h")),
            o => panic!("{o:?}"),
        }
        assert!(matches!(classify("ssh: Could not resolve hostname x", 255, "x"), Some(SshError::Unreachable(_))));
        assert!(matches!(classify("Connection refused", 255, "x"), Some(SshError::Unreachable(_))));
        assert!(classify("whatever", 1, "x").is_none());
    }

    #[test]
    fn askpass_env_gui_and_headless() {
        let g = askpass_env(true);
        assert!(g.contains(&("SSH_ASKPASS_REQUIRE".into(), "force".into())));
        assert!(g.contains(&("ARCUS_ASKPASS".into(), "1".into())));
        assert!(g.iter().any(|(k, _)| k == "SSH_ASKPASS"));
        assert_eq!(askpass_env(false), vec![("SSH_ASKPASS_REQUIRE".to_string(), "never".to_string())]);
    }

    #[test]
    fn scp_uses_capital_p_for_explicit_port() {
        let home = tmp_home("scp");
        let fake = Fake::new(vec![]);
        let m = explicit();
        let ssh = Ssh { home: &home, runner: &fake, machine: &m, policy: HostKeyPolicy::AcceptNew };
        ssh.copy(Path::new("/tmp/f"), "/remote/f").unwrap();
        let calls = fake.calls.borrow();
        let (prog, argv, _) = &calls[0];
        assert_eq!(prog, "scp");
        let i = argv.iter().position(|a| a == "-P").unwrap();
        assert_eq!(argv[i + 1], "2222");
        assert!(!argv.contains(&"-p".to_string()));
        assert!(argv.contains(&"StrictHostKeyChecking=accept-new".to_string()));
        assert_eq!(&argv[argv.len() - 2..], ["/tmp/f", "u@h.example:/remote/f"]);
        let _ = std::fs::remove_dir_all(&home.root);
    }

    #[test]
    fn unknown_host_key_gets_fingerprint() {
        let home = tmp_home("fp");
        let fake = Fake::new(vec![
            SshOutput { status: 255, stdout: vec![], stderr: "Host key verification failed.".into() },
            SshOutput { status: 0, stdout: b"hostname 10.0.0.5\nport 2200\n".to_vec(), stderr: String::new() },
            SshOutput { status: 0, stdout: b"10.0.0.5 ssh-ed25519 AAAA\n".to_vec(), stderr: String::new() },
            SshOutput { status: 0, stdout: b"256 SHA256:abc 10.0.0.5 (ED25519)\n".to_vec(), stderr: String::new() },
        ]);
        let m = alias("dev-1");
        let ssh = Ssh { home: &home, runner: &fake, machine: &m, policy: HostKeyPolicy::Strict };
        match ssh.exec("true", b"") {
            Err(SshError::HostKeyUnknown { fingerprint }) => assert!(fingerprint.contains("SHA256:abc")),
            o => panic!("{o:?}"),
        }
        let calls = fake.calls.borrow();
        assert_eq!(calls[2].1, ["-p", "2200", "10.0.0.5"]);
        let _ = std::fs::remove_dir_all(&home.root);
    }

    #[test]
    fn remote_nonzero_status_is_ok() {
        let home = tmp_home("rc");
        let fake = Fake::new(vec![SshOutput { status: 3, stdout: vec![], stderr: "x".into() }]);
        let m = alias("a");
        let ssh = Ssh { home: &home, runner: &fake, machine: &m, policy: HostKeyPolicy::Strict };
        assert_eq!(ssh.exec("false", b"").unwrap().status, 3);
        let _ = std::fs::remove_dir_all(&home.root);
    }
}
