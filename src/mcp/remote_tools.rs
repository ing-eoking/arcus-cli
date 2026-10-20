//! Machine tools: list ssh-config aliases, register machines, check a machine over SSH.
//! Each tool's logic is a function over `&dyn Runner` so it can be tested without ssh.

use super::{arg_bool, arg_opt_str, arg_str, arg_u64, Ctx, Tool, ToolResult, ToolSpec};
use crate::remote::machines::{load_machines, resolve, save_machine, ssh_config_aliases, Machine, Target};
use crate::remote::forward::ssh_error_result;
use crate::remote::ssh::{HostKeyPolicy, Runner, Ssh, SystemRunner};
use crate::server::home::Home;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub fn registry() -> Vec<Box<dyn Tool>> {
    vec![Box::new(HostListSshConfig), Box::new(HostAdd), Box::new(HostList), Box::new(HostCheck)]
}

fn ssh_config_path() -> PathBuf {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".ssh/config")).unwrap_or_else(|| PathBuf::from(".ssh/config"))
}

pub fn target_for(uname_s: &str, uname_m: &str) -> Option<&'static str> {
    match (uname_s, uname_m) {
        ("Linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("Linux", "aarch64") | ("Linux", "arm64") => Some("aarch64-unknown-linux-gnu"),
        ("Darwin", "x86_64") => Some("x86_64-apple-darwin"),
        ("Darwin", "arm64") => Some("aarch64-apple-darwin"),
        _ => None,
    }
}

macro_rules! try_arg {
    ($e:expr) => { match $e { Ok(v) => v, Err(r) => return r } };
}

pub fn host_list_ssh_config(config: &Path) -> ToolResult {
    let aliases = ssh_config_aliases(config).unwrap_or_default();
    ToolResult::ok(
        format!("{} alias(es) in ~/.ssh/config: {}", aliases.len(), aliases.join(", ")),
        Some(json!({"aliases": aliases})),
    )
}

/// `ssh -G <name>` -> (hostname, user, port).
fn resolve_alias(runner: &dyn Runner, name: &str) -> Option<(String, String, u16)> {
    let out = runner.run("ssh", &["-G".into(), name.into()], &[], &[]).ok()?;
    if out.status != 0 { return None; }
    let text = String::from_utf8_lossy(&out.stdout);
    let (mut host, mut user, mut port) = (None, None, 22u16);
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("hostname ") { host = Some(v.trim().to_string()); }
        if let Some(v) = line.strip_prefix("user ") { user = Some(v.trim().to_string()); }
        if let Some(v) = line.strip_prefix("port ") { port = v.trim().parse().unwrap_or(22); }
    }
    Some((host?, user.unwrap_or_default(), port))
}

pub fn host_add(home: &Home, runner: &dyn Runner, config: &Path, args: &Value) -> ToolResult {
    let name = try_arg!(arg_str(args, "name"));
    let host = arg_opt_str(args, "host");
    let aliases = ssh_config_aliases(config).unwrap_or_default();
    if host.is_none() && aliases.contains(&name) {
        let m = Machine { name: name.clone(), target: Target::SshAlias };
        if let Err(e) = save_machine(home, &m) { return ToolResult::err(e.to_string()); }
        return match resolve_alias(runner, &name) {
            Some((h, u, p)) => ToolResult::ok(
                format!("registered ssh alias `{name}` ({u}@{h}:{p})"),
                Some(json!({"machine": name, "kind": "ssh_alias", "hostname": h, "user": u, "port": p}))),
            None => ToolResult::ok(
                format!("registered ssh alias `{name}` (could not resolve details with ssh -G)"),
                Some(json!({"machine": name, "kind": "ssh_alias"}))),
        };
    }
    let host = try_arg!(arg_str(args, "host"));
    let user = try_arg!(arg_str(args, "user"));
    let port = arg_u64(args, "port").unwrap_or(22);
    let Ok(port) = u16::try_from(port) else { return ToolResult::err(format!("invalid port {port}")) };
    let identity_file = arg_opt_str(args, "identity_file");
    let m = Machine { name: name.clone(), target: Target::Explicit { host: host.clone(), user: user.clone(), port, identity_file } };
    if let Err(e) = save_machine(home, &m) { return ToolResult::err(e.to_string()); }
    ToolResult::ok(
        format!("registered machine `{name}` ({user}@{host}:{port})"),
        Some(json!({"machine": name, "kind": "explicit", "hostname": host, "user": user, "port": port})))
}

pub fn host_list(home: &Home) -> ToolResult {
    let machines = match load_machines(home) { Ok(m) => m, Err(e) => return ToolResult::err(e.to_string()) };
    let list: Vec<Value> = machines.iter().map(|m| match &m.target {
        Target::SshAlias => json!({"name": m.name, "kind": "ssh_alias"}),
        Target::Explicit { host, user, port, identity_file } =>
            json!({"name": m.name, "kind": "explicit", "host": host, "user": user, "port": port, "identity_file": identity_file}),
    }).collect();
    ToolResult::ok(format!("{} registered machine(s)", list.len()), Some(json!({"machines": list})))
}

const CHECK_CMD: &str = "uname -sm; echo $HOME; ~/.arcus-cli/bin/arcus-cli --version 2>/dev/null; \
test -S ~/.arcus-cli/agent.sock && ~/.arcus-cli/bin/arcus-cli --agent-status --home ~/.arcus-cli";

pub fn parse_check(stdout: &str) -> Result<Value, String> {
    let mut lines = stdout.lines();
    let uname = lines.next().ok_or("empty output from machine")?;
    let mut it = uname.split_whitespace();
    let (os, arch) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
    let home = lines.next().unwrap_or("").trim().to_string();
    let (mut arcus_cli, mut agent) = (Value::Null, Value::Null);
    for line in lines {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("arcus-cli ") {
            let version = rest.split_whitespace().next().unwrap_or("");
            let tag = rest.split_once('(').and_then(|(_, t)| t.split_once(')')).map(|(t, _)| t).unwrap_or("");
            arcus_cli = json!({"version": version, "tag": tag});
        } else if let Some(v) = serde_json::from_str::<Value>(line).ok().filter(Value::is_object) {
            agent = json!({
                "running": v.get("running").and_then(Value::as_bool).unwrap_or(true),
                "servers": v.get("servers").cloned().unwrap_or_else(|| json!([])),
            });
        }
    }
    Ok(json!({"os": os, "arch": arch, "target": target_for(os, arch), "home": home, "arcus_cli": arcus_cli, "agent": agent}))
}

pub fn host_check(home: &Home, runner: &dyn Runner, config: &Path, args: &Value) -> ToolResult {
    let name = try_arg!(arg_str(args, "machine"));
    let machine = match resolve(home, &name, config) { Ok(m) => m, Err(e) => return ToolResult::err(e) };
    let policy = if arg_bool(args, "accept_host_key", false) { HostKeyPolicy::AcceptNew } else { HostKeyPolicy::Strict };
    let ssh = Ssh { home, runner, machine: &machine, policy };
    let out = match ssh.exec(CHECK_CMD, b"") { Ok(o) => o, Err(e) => return ssh_error_result(e) };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut v = match parse_check(&text) { Ok(v) => v, Err(e) => return ToolResult::err(e) };
    v["machine"] = json!(name);
    let summary = format!("{name}: {} {} (target {}), arcus-cli {}, agent {}",
        v["os"].as_str().unwrap_or("?"), v["arch"].as_str().unwrap_or("?"),
        v["target"].as_str().unwrap_or("unsupported"),
        if v["arcus_cli"].is_null() { "not installed" } else { "installed" },
        if v["agent"].is_null() { "not running" } else { "running" });
    ToolResult::ok(summary, Some(v))
}

struct HostListSshConfig;
impl Tool for HostListSshConfig {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "arcus_host_list_ssh_config",
            description: "List the machine names (Host aliases) in ~/.ssh/config. Machine names come from ~/.ssh/config or arcus_host_add.",
            input_schema: json!({"type": "object", "properties": {}}),
        }
    }
    fn call(&self, _ctx: &Ctx, _args: &Value) -> ToolResult { host_list_ssh_config(&ssh_config_path()) }
}

struct HostAdd;
impl Tool for HostAdd {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "arcus_host_add",
            description: "Register a machine for remote tools. A name already in ~/.ssh/config needs no other details. \
Otherwise host and user are required: ask the user for the connection details (port recommended 22) and confirm them \
before calling. Passwords are never stored.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "Machine name (an ~/.ssh/config alias, or a new name)"},
                    "host": {"type": "string", "description": "Hostname or IP (not needed for an ssh config alias)"},
                    "user": {"type": "string", "description": "SSH user"},
                    "port": {"type": "integer", "description": "SSH port (recommended 22)"},
                    "identity_file": {"type": "string", "description": "Optional private key path"}
                },
                "required": ["name"]
            }),
        }
    }
    fn call(&self, ctx: &Ctx, args: &Value) -> ToolResult { host_add(&ctx.home, &SystemRunner, &ssh_config_path(), args) }
}

struct HostList;
impl Tool for HostList {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "arcus_host_list",
            description: "List machines registered with arcus_host_add (machines from ~/.ssh/config are listed by arcus_host_list_ssh_config).",
            input_schema: json!({"type": "object", "properties": {}}),
        }
    }
    fn call(&self, ctx: &Ctx, _args: &Value) -> ToolResult { host_list(&ctx.home) }
}

struct HostCheck;
impl Tool for HostCheck {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "arcus_host_check",
            description: "Connect to a machine over SSH and report its OS, architecture, home, installed arcus-cli and agent state. \
The machine name comes from ~/.ssh/config or arcus_host_add. An unknown host key returns a fingerprint: show it to the user \
and call again with accept_host_key: true only after the user confirms.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "machine": {"type": "string", "description": "Machine name"},
                    "accept_host_key": {"type": "boolean", "description": "Accept a new host key (only after the user confirmed the fingerprint; default false)"}
                },
                "required": ["machine"]
            }),
        }
    }
    fn call(&self, ctx: &Ctx, args: &Value) -> ToolResult { host_check(&ctx.home, &SystemRunner, &ssh_config_path(), args) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::ssh::SshOutput;
    use std::cell::RefCell;
    use std::io;

    struct Fake { calls: RefCell<Vec<(String, Vec<String>)>>, reply: RefCell<Vec<SshOutput>> }
    impl Fake {
        fn new(reply: Vec<SshOutput>) -> Fake { Fake { calls: RefCell::new(vec![]), reply: RefCell::new(reply) } }
    }
    impl Runner for Fake {
        fn run(&self, p: &str, a: &[String], _i: &[u8], _e: &[(String, String)]) -> io::Result<SshOutput> {
            self.calls.borrow_mut().push((p.into(), a.to_vec()));
            let mut r = self.reply.borrow_mut();
            Ok(if r.is_empty() { SshOutput { status: 0, stdout: vec![], stderr: String::new() } } else { r.remove(0) })
        }
    }
    fn out(status: i32, stdout: &str, stderr: &str) -> SshOutput {
        SshOutput { status, stdout: stdout.as_bytes().to_vec(), stderr: stderr.into() }
    }
    fn tmp(tag: &str) -> (Home, PathBuf) {
        let d = std::env::temp_dir().join(format!("arcus-rt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let cfg = d.join("config");
        std::fs::write(&cfg, "Host dev-1\n  HostName 10.0.0.1\n").unwrap();
        (Home { root: d.join("home") }, cfg)
    }

    #[test]
    fn target_mapping() {
        assert_eq!(target_for("Linux", "aarch64"), Some("aarch64-unknown-linux-gnu"));
        assert_eq!(target_for("Darwin", "arm64"), Some("aarch64-apple-darwin"));
        assert_eq!(target_for("Linux", "x86_64"), Some("x86_64-unknown-linux-gnu"));
        assert_eq!(target_for("SunOS", "x"), None);
    }

    #[test]
    fn host_add_alias_needs_no_details() {
        let (home, cfg) = tmp("alias");
        std::fs::create_dir_all(&home.root).unwrap();
        let fake = Fake::new(vec![out(0, "hostname 10.0.0.1\nuser bob\nport 2200\n", "")]);
        let r = host_add(&home, &fake, &cfg, &json!({"name": "dev-1"}));
        assert!(!r.is_error, "{}", r.text);
        let s = r.structured.unwrap();
        assert_eq!((s["hostname"].as_str(), s["user"].as_str(), s["port"].as_u64()), (Some("10.0.0.1"), Some("bob"), Some(2200)));
        assert_eq!(load_machines(&home).unwrap()[0].target, Target::SshAlias);
    }

    #[test]
    fn host_add_explicit_requires_user() {
        let (home, cfg) = tmp("explicit");
        std::fs::create_dir_all(&home.root).unwrap();
        let fake = Fake::new(vec![]);
        let r = host_add(&home, &fake, &cfg, &json!({"name": "new", "host": "h.example"}));
        assert!(r.is_error && r.text.contains("ask the user"));
        let r = host_add(&home, &fake, &cfg, &json!({"name": "new", "host": "h.example", "user": "u"}));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(load_machines(&home).unwrap().len(), 1);
    }

    #[test]
    fn host_check_parses_output() {
        let (home, cfg) = tmp("check");
        let fake = Fake::new(vec![out(0, "Linux aarch64\n/home/u\narcus-cli 1.0.0 (v1.1.0)\n", "")]);
        let r = host_check(&home, &fake, &cfg, &json!({"machine": "dev-1"}));
        assert!(!r.is_error, "{}", r.text);
        let s = r.structured.unwrap();
        assert_eq!(s["os"], "Linux");
        assert_eq!(s["target"], "aarch64-unknown-linux-gnu");
        assert_eq!(s["home"], "/home/u");
        assert_eq!(s["arcus_cli"]["tag"], "v1.1.0");
        assert_eq!(s["arcus_cli"]["version"], "1.0.0");
        assert!(s["agent"].is_null());
        assert_eq!(s["machine"], "dev-1");
    }

    #[test]
    fn host_check_unknown_key_returns_fingerprint() {
        let (home, cfg) = tmp("fp");
        let fake = Fake::new(vec![
            out(255, "", "Host key verification failed."),
            out(0, "hostname 10.0.0.1\nport 22\n", ""),
            out(0, "10.0.0.1 ssh-ed25519 AAAA\n", ""),
            out(0, "256 SHA256:abc 10.0.0.1 (ED25519)\n", ""),
        ]);
        let r = host_check(&home, &fake, &cfg, &json!({"machine": "dev-1"}));
        assert!(r.is_error);
        assert!(r.text.contains("SHA256:abc") && r.text.contains("accept_host_key: true"));
    }
}
