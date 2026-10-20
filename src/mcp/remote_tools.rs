//! Machine tools: list ssh-config aliases, register machines, check a machine over SSH.
//! Each tool's logic is a function over `&dyn Runner` so it can be tested without ssh.

use super::{arg_bool, arg_opt_str, arg_str, arg_u64, Ctx, Tool, ToolResult, ToolSpec};
use crate::remote::machines::{load_machines, resolve, save_machine, ssh_config_aliases, Machine, Target};
use crate::remote::forward::{forward, ssh_error_result};
use crate::remote::ssh::{HostKeyPolicy, Runner, Ssh, SystemRunner};
use crate::server::home::Home;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub fn registry() -> Vec<Box<dyn Tool>> {
    vec![Box::new(HostListSshConfig), Box::new(HostAdd), Box::new(HostList), Box::new(HostCheck), Box::new(HostInstall), Box::new(ClusterZkPlan), Box::new(ClusterZkStart)]
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

struct HostInstall;
impl Tool for HostInstall {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "arcus_host_install",
            description: "Copy arcus-cli to a machine and keep its resident agent running (systemd user service / launchd, or a daemon). \
Without apply it only returns a plan: show it to the user and call again with apply: true after they agree. \
If an agent already runs there, it stops and returns its version and managed servers: ask the user, then use replace: true \
(only the agent is replaced; servers keep running). On Linux, service mode needs linger; if it is refused, ask the user to choose \
mode: \"daemon\" or an administrator running `sudo loginctl enable-linger <user>`. Never run sudo.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "machine": {"type": "string", "description": "Machine name"},
                    "mode": {"type": "string", "enum": ["service", "daemon"], "description": "How the agent stays up (recommended service)"},
                    "tag": {"type": "string", "description": "Release tag to install (recommended: this build's tag)"},
                    "binary": {"type": "string", "description": "Local arcus-cli binary to ship instead of a release (development)"},
                    "replace": {"type": "boolean", "description": "Replace a running agent (default false)"},
                    "apply": {"type": "boolean", "description": "Perform the install (default false: plan only)"},
                    "accept_host_key": {"type": "boolean", "description": "Accept a new host key (only after the user confirmed the fingerprint)"}
                },
                "required": ["machine"]
            }),
        }
    }
    fn call(&self, ctx: &Ctx, args: &Value) -> ToolResult {
        crate::remote::install::host_install(&ctx.home, &SystemRunner, &ssh_config_path(), args, std::time::Duration::from_secs(1))
    }
}

// ---------------------------------------------------------------- ZooKeeper across machines

const CLUSTER_ZK_WAIT: std::time::Duration = std::time::Duration::from_secs(60);
const CLUSTER_ZK_POLL: std::time::Duration = std::time::Duration::from_secs(2);

/// First address printed by `hostname -I` (Linux) or `ipconfig getifaddr en0` (macOS) on `machine`.
fn machine_address(home: &Home, runner: &dyn Runner, config: &Path, machine: &str) -> Result<String, String> {
    let m = resolve(home, machine, config)?;
    let ssh = Ssh { home, runner, machine: &m, policy: HostKeyPolicy::Strict };
    let o = ssh.exec("hostname -I 2>/dev/null || ipconfig getifaddr en0", b"").map_err(|e| ssh_error_result(e).text)?;
    String::from_utf8_lossy(&o.stdout).split_whitespace().next().map(str::to_string)
        .ok_or_else(|| format!("could not determine an address of `{machine}`; ask the user for it"))
}

fn port_field(n: &Value, key: &str, default: u64, occurrence: u64) -> Result<u16, String> {
    let p = n.get(key).and_then(Value::as_u64).unwrap_or(default + occurrence);
    u16::try_from(p).ok().filter(|p| *p != 0).ok_or_else(|| format!("invalid `{key}` {p}"))
}

/// Recommended ensemble for `arcus_cluster_zk_plan`: one node per machine unless `nodes` is given; a missing
/// address is the machine's first `hostname -I` address. Nothing is written anywhere.
pub fn cluster_zk_plan(home: &Home, runner: &dyn Runner, config: &Path, args: &Value) -> ToolResult {
    let entries: Vec<Value> = if let Some(nodes) = args.get("nodes").and_then(Value::as_array).filter(|a| !a.is_empty()) {
        nodes.clone()
    } else {
        let names: Vec<String> = match args.get("machines").and_then(Value::as_array).filter(|a| !a.is_empty()) {
            Some(a) => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
            None => match load_machines(home) { Ok(m) => m.into_iter().map(|m| m.name).collect(), Err(e) => return ToolResult::err(e.to_string()) },
        };
        names.into_iter().map(|m| json!({"machine": m})).collect()
    };
    if entries.is_empty() { return ToolResult::err("no machines given and none registered; call arcus_host_add first or pass `machines`"); }
    if entries.len() > 7 { return ToolResult::err("a ZooKeeper ensemble has at most 7 nodes here"); }
    let mut seen: std::collections::BTreeMap<String, u64> = Default::default();
    let mut nodes = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let Some(machine) = e.get("machine").and_then(Value::as_str).filter(|m| !m.is_empty()) else {
            return ToolResult::err(format!("node {} has no `machine`", i + 1));
        };
        let occ = { let c = seen.entry(machine.to_string()).or_insert(0); let v = *c; *c += 1; v };
        let (client, peer, election) = match (port_field(e, "client_port", 2181, occ), port_field(e, "peer_port", 2888, occ), port_field(e, "election_port", 3888, occ)) {
            (Ok(a), Ok(b), Ok(c)) => (a, b, c),
            (Err(m), _, _) | (_, Err(m), _) | (_, _, Err(m)) => return ToolResult::err(m),
        };
        let address = match e.get("address").and_then(Value::as_str).filter(|a| !a.is_empty()) {
            Some(a) => a.to_string(),
            None => match machine_address(home, runner, config, machine) { Ok(a) => a, Err(m) => return ToolResult::err(m) },
        };
        nodes.push(json!({"id": i + 1, "machine": machine, "address": address,
            "client_port": client, "peer_port": peer, "election_port": election}));
    }
    let ensemble = nodes.iter().map(|n| format!("{}:{}", n["address"].as_str().unwrap_or(""), n["client_port"])).collect::<Vec<_>>().join(",");
    ToolResult::ok(
        format!("Proposed ZooKeeper ensemble ({} node(s)): {ensemble}. Show it to the user (addresses are recommendations; \
peer and election ports must be reachable between the machines) and call arcus_cluster_zk_start with these nodes after they agree.", nodes.len()),
        Some(json!({"nodes": nodes, "ensemble": ensemble})))
}

/// Starts the ensemble described by `args.nodes`: forwards `arcus_zk_start {members, my_ids}` to every machine,
/// then polls `arcus_zk_mode` on each until all serve or `wait` passes. `fwd(tool, args, machine)` reaches a machine.
pub fn cluster_zk_start(args: &Value, fwd: &dyn Fn(&str, &Value, &str) -> ToolResult, wait: std::time::Duration, poll: std::time::Duration) -> ToolResult {
    let Some(nodes) = args.get("nodes").and_then(Value::as_array).filter(|a| !a.is_empty()) else {
        return ToolResult::err("missing argument `nodes`; call arcus_cluster_zk_plan first");
    };
    let mut parsed = Vec::new();
    for n in nodes {
        let get = |k: &str| n.get(k).and_then(Value::as_u64);
        let (Some(id), Some(c), Some(p), Some(e), Some(machine), Some(address)) = (get("id"), get("client_port"), get("peer_port"), get("election_port"),
            n.get("machine").and_then(Value::as_str).filter(|s| !s.is_empty()), n.get("address").and_then(Value::as_str).filter(|s| !s.is_empty())) else {
            return ToolResult::err("every node needs id, machine, address, client_port, peer_port and election_port");
        };
        parsed.push((id, machine.to_string(), address.to_string(), c, p, e));
    }
    let members: Vec<Value> = parsed.iter().map(|(id, _, a, c, p, e)| json!({"id": id, "address": a, "client_port": c, "peer_port": p, "election_port": e})).collect();
    let ensemble = parsed.iter().map(|(_, _, a, c, ..)| format!("{a}:{c}")).collect::<Vec<_>>().join(",");
    let mut machines: Vec<&str> = Vec::new();
    for (_, m, ..) in &parsed { if !machines.contains(&m.as_str()) { machines.push(m); } }
    let mut jobs = Vec::new();
    for m in &machines {
        let my_ids: Vec<u64> = parsed.iter().filter(|p| p.1 == *m).map(|p| p.0).collect();
        let mut a = json!({"members": members, "my_ids": my_ids});
        if let Some(v) = args.get("version").filter(|v| !v.is_null()) { a["version"] = v.clone(); }
        let r = fwd("arcus_zk_start", &a, m);
        if r.is_error { return ToolResult::err(format!("starting ZooKeeper on `{m}` failed: {}", r.text)); }
        if let Some(job) = r.structured.as_ref().and_then(|s| s.get("job_id")) { jobs.push(json!({"machine": m, "job_id": job})); }
    }
    if !jobs.is_empty() {
        return ToolResult::ok(
            "ZooKeeper is being downloaded on some machines (jobs below). Poll arcus_job_status with each job_id and machine until succeeded, \
then call arcus_cluster_zk_start again with the same nodes.", Some(json!({"jobs": jobs})));
    }
    let deadline = std::time::Instant::now() + wait;
    let mut modes: Vec<Option<String>> = vec![None; parsed.len()];
    loop {
        for (i, (_, m, _, c, ..)) in parsed.iter().enumerate() {
            if modes[i].is_some() { continue; }
            let r = fwd("arcus_zk_mode", &json!({"port": c}), m);
            modes[i] = r.structured.as_ref().and_then(|s| s["mode"].as_str()).map(str::to_string);
        }
        if modes.iter().all(Option::is_some) || std::time::Instant::now() >= deadline { break; }
        std::thread::sleep(poll);
    }
    let list: Vec<Value> = parsed.iter().zip(&modes).map(|((id, m, ..), mode)| json!({"id": id, "machine": m, "mode": mode})).collect();
    if modes.iter().all(Option::is_some) {
        return ToolResult::ok(format!("ZooKeeper ensemble is running at {ensemble}."), Some(json!({"ensemble": ensemble, "nodes": list})));
    }
    let stuck: Vec<String> = parsed.iter().zip(&modes).filter(|(_, m)| m.is_none()).map(|(p, _)| format!("{} on `{}`", p.0, p.1)).collect();
    let mut res = ToolResult::err(format!(
        "ZooKeeper node(s) {} did not start serving within {} s. The processes were started but have no mode, which usually means the \
peer/election ports between the machines are blocked (firewall or security group): open the peer and election ports between all machines, \
or check each node's log under ~/.arcus-cli/zk/node<id>/zookeeper.log. Nodes that already serve: {}.",
        stuck.join(", "), wait.as_secs(),
        parsed.iter().zip(&modes).filter_map(|(p, m)| m.as_ref().map(|m| format!("{} ({m})", p.0))).collect::<Vec<_>>().join(", ")));
    res.structured = Some(json!({"ensemble": ensemble, "nodes": list}));
    res
}

struct ClusterZkPlan;
impl Tool for ClusterZkPlan {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "arcus_cluster_zk_plan",
            description: "Propose a ZooKeeper ensemble spread over several machines (one node per machine, ports 2181/2888/3888, address = the machine's \
first IP from hostname -I). Only returns the plan; show it to the user and let them change addresses or ports, then call arcus_cluster_zk_start.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "machines": {"type": "array", "items": {"type": "string"}, "description": "Machine names, one node each (recommended: all registered machines)"},
                    "nodes": {"type": "array", "description": "Explicit nodes [{machine, client_port?, peer_port?, election_port?, address?}]; overrides machines",
                        "items": {"type": "object"}},
                    "version": {"type": "string", "description": "ZooKeeper version (recommended 3.8.6)"}
                }
            }),
        }
    }
    fn call(&self, ctx: &Ctx, args: &Value) -> ToolResult { cluster_zk_plan(&ctx.home, &SystemRunner, &ssh_config_path(), args) }
}

struct ClusterZkStart;
impl Tool for ClusterZkStart {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "arcus_cluster_zk_start",
            description: "Start the ZooKeeper ensemble confirmed with the user (nodes as returned by arcus_cluster_zk_plan) on several machines and wait until \
every node serves. If ZooKeeper must be downloaded first, returns jobs to poll, then call again. Returns the ensemble connect string for arcus_setup.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "nodes": {"type": "array", "description": "[{id, machine, address, client_port, peer_port, election_port}]", "items": {"type": "object"}},
                    "version": {"type": "string", "description": "ZooKeeper version (recommended 3.8.6)"}
                },
                "required": ["nodes"]
            }),
        }
    }
    fn call(&self, ctx: &Ctx, args: &Value) -> ToolResult {
        let fwd = |tool: &str, a: &Value, machine: &str| forward(ctx, tool, a, machine, &SystemRunner);
        cluster_zk_start(args, &fwd, CLUSTER_ZK_WAIT, CLUSTER_ZK_POLL)
    }
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

    #[test]
    fn plan_one_node_per_machine() {
        let (home, cfg) = tmp("plan");
        let fake = Fake::new(vec![out(0, "10.0.0.5 172.17.0.1\n", "")]);
        let r = cluster_zk_plan(&home, &fake, &cfg, &json!({"machines": ["dev-1"]}));
        assert!(!r.is_error, "{}", r.text);
        let s = r.structured.unwrap();
        let n = &s["nodes"][0];
        assert_eq!((n["id"].as_u64(), n["address"].as_str(), n["client_port"].as_u64(), n["peer_port"].as_u64(), n["election_port"].as_u64()),
            (Some(1), Some("10.0.0.5"), Some(2181), Some(2888), Some(3888)));
        assert_eq!(s["ensemble"], "10.0.0.5:2181");
        assert!(fake.calls.borrow()[0].1.iter().any(|a| a.contains("hostname -I")));
    }

    fn start_args() -> Value {
        json!({"nodes": [
            {"id": 1, "machine": "a", "address": "10.0.0.1", "client_port": 2181, "peer_port": 2888, "election_port": 3888},
            {"id": 2, "machine": "b", "address": "10.0.0.2", "client_port": 2181, "peer_port": 2888, "election_port": 3888},
            {"id": 3, "machine": "c", "address": "10.0.0.3", "client_port": 2181, "peer_port": 2888, "election_port": 3888}]})
    }

    #[test]
    fn cluster_start_reports_stuck_nodes() {
        let started = RefCell::new(Vec::new());
        let fwd = |tool: &str, a: &Value, m: &str| -> ToolResult {
            if tool == "arcus_zk_start" { started.borrow_mut().push((m.to_string(), a.clone())); return ToolResult::ok("ok", Some(json!({}))); }
            let mode = if m == "c" { Value::Null } else { json!("follower") };
            ToolResult::ok("m", Some(json!({"mode": mode})))
        };
        let r = cluster_zk_start(&start_args(), &fwd, std::time::Duration::from_millis(30), std::time::Duration::from_millis(5));
        assert!(r.is_error);
        assert!(r.text.contains("3 on `c`") && r.text.contains("blocked"), "{}", r.text);
        assert!(!r.text.contains("3 on `a`"));
        let st = started.borrow();
        assert_eq!(st.len(), 3);
        assert_eq!(st[1].1["my_ids"], json!([2]));
        assert_eq!(st[1].1["members"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn cluster_start_success_returns_ensemble() {
        let fwd = |tool: &str, _a: &Value, _m: &str| -> ToolResult {
            if tool == "arcus_zk_start" { ToolResult::ok("ok", Some(json!({}))) } else { ToolResult::ok("m", Some(json!({"mode": "leader"}))) }
        };
        let r = cluster_zk_start(&start_args(), &fwd, std::time::Duration::from_millis(30), std::time::Duration::from_millis(5));
        assert!(!r.is_error, "{}", r.text);
        let s = r.structured.unwrap();
        assert_eq!(s["ensemble"], "10.0.0.1:2181,10.0.0.2:2181,10.0.0.3:2181");
        assert_eq!(s["nodes"][0]["mode"], "leader");
    }

    #[test]
    fn cluster_start_surfaces_download_jobs() {
        let fwd = |_t: &str, _a: &Value, m: &str| -> ToolResult { ToolResult::ok("job", Some(json!({"job_id": format!("j-{m}")}))) };
        let r = cluster_zk_start(&start_args(), &fwd, std::time::Duration::from_millis(30), std::time::Duration::from_millis(5));
        assert!(!r.is_error);
        assert_eq!(r.structured.unwrap()["jobs"][2], json!({"machine": "c", "job_id": "j-c"}));
    }
}
