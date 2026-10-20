use super::{arg_bool, arg_opt_str, arg_str, arg_u64, Ctx, Tool, ToolResult, ToolSpec};
use crate::connect::collect_responses;
use crate::remote::forward::Remote;
use crate::server::build::{detect_os, install_hint, missing_tools, on_path};
use crate::server::features::{Edition, Feature};
use crate::server::release::{self, find_gh, find_tokens};
use crate::server::secret::{random_password, write_secret};
use crate::zk::{zk_exec, ZkCommand};
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

const MAX_WAIT: Duration = Duration::from_secs(10);
const MAX_BYTES: usize = 1 << 20;

/// All tools exposed by `arcus-cli --mcp` (later tasks append here).
pub fn registry() -> Vec<Box<dyn Tool>> {
    let mut v: Vec<Box<dyn Tool>> = vec![
        Box::new(Remote(MemcachedCommand)),
        Box::new(Remote(ZkLs)),
        Box::new(Remote(ZkGet)),
        Box::new(Remote(ZkCreate)),
        Box::new(Remote(ZkSet)),
        Box::new(Remote(ZkPassword)),
        Box::new(Remote(ListFeatures)),
        Box::new(Remote(CheckPrerequisites)),
        Box::new(Remote(JobStatusTool)),
        Box::new(Remote(ListVersions)),
        Box::new(Remote(EeLogin)),
        Box::new(Remote(Install)),
        Box::new(Remote(ZkStart)),
        Box::new(Remote(ZkStop)),
        Box::new(Remote(ZkMode)),
        Box::new(Remote(Setup)),
        Box::new(Remote(Start)),
        Box::new(Remote(Stop)),
        Box::new(Remote(StatusTool)),
    ];
    v.extend(super::remote_tools::registry());
    v
}

macro_rules! try_arg {
    ($e:expr) => { match $e { Ok(v) => v, Err(r) => return r } };
}

struct MemcachedCommand;
impl Tool for MemcachedCommand {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "memcached_command",
            description: "Send ASCII commands to an ARCUS/memcached server and return its response. Arguments with recommended \
                values: propose them to the user and confirm before calling. Multiple commands and data lines are separated by newlines, e.g. \"set a 0 0 1\\na\\nget a\".",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "host": {"type": "string", "description": "Server host (recommended 127.0.0.1)"},
                    "port": {"type": "integer", "description": "Server port (recommended 11211)"},
                    "unix_path": {"type": "string", "description": "Unix socket path; used instead of host/port"},
                    "command": {"type": "string", "description": "Commands separated by newlines, data lines included"},
                    "idle_ms": {"type": "integer", "description": "Wait this long without new output before returning (recommended 300)"}
                },
                "required": ["command"]
            }),
        }
    }
    fn call(&self, _ctx: &Ctx, args: &Value) -> ToolResult {
        let command = try_arg!(arg_str(args, "command"));
        let idle = idle_of(args);
        let (addr, unix) = match arg_opt_str(args, "unix_path") {
            Some(p) => (p, true),
            None => {
                let host = arg_opt_str(args, "host").unwrap_or_else(|| "127.0.0.1".into());
                let port = arg_u64(args, "port").unwrap_or(11211);
                (format!("{host}:{port}"), false)
            }
        };
        match collect_responses(&addr, unix, &command, idle, MAX_WAIT, MAX_BYTES) {
            Ok(text) => ToolResult::ok(text, None),
            Err(e) => ToolResult::err(format!("memcached_command failed: {e}")),
        }
    }
}

/// Idle wait from `idle_ms`, never longer than the overall limit.
fn idle_of(args: &Value) -> Duration {
    Duration::from_millis(arg_u64(args, "idle_ms").unwrap_or(300)).min(MAX_WAIT)
}

/// Runs a ZooKeeper command; output lines starting with `ERROR: ` make the result an error.
fn run_zk(ensemble: &str, cmd: ZkCommand) -> ToolResult {
    match zk_exec(ensemble, cmd) {
        Ok(text) if text.lines().any(|l| l.starts_with("ERROR: ")) => ToolResult::err(text),
        Ok(text) => ToolResult::ok(text, None),
        Err(e) => ToolResult::err(e.to_string()),
    }
}

fn zk_schema(extra: &[(&str, &str)], required: &[&str]) -> Value {
    let mut props = json!({
        "ensemble": {"type": "string", "description": "ZooKeeper address, e.g. 127.0.0.1:2181"},
        "path": {"type": "string", "description": "Znode path, e.g. /arcus"}
    });
    for (k, d) in extra { props[*k] = json!({"type": if *k == "recursive" || *k == "overwrite" {"boolean"} else {"string"}, "description": d}); }
    json!({"type": "object", "properties": props, "required": required})
}

struct ZkLs;
impl Tool for ZkLs {
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: "zk_ls", description: "List the children of a ZooKeeper znode, optionally recursively.",
            input_schema: zk_schema(&[("recursive", "List all descendants (default false)")], &["ensemble", "path"]) }
    }
    fn call(&self, _ctx: &Ctx, args: &Value) -> ToolResult {
        let ensemble = try_arg!(arg_str(args, "ensemble"));
        let path = try_arg!(arg_str(args, "path"));
        run_zk(&ensemble, ZkCommand::Ls { path, recursive: arg_bool(args, "recursive", false), stat: false })
    }
}

struct ZkGet;
impl Tool for ZkGet {
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: "zk_get", description: "Read the data of a ZooKeeper znode.",
            input_schema: zk_schema(&[], &["ensemble", "path"]) }
    }
    fn call(&self, _ctx: &Ctx, args: &Value) -> ToolResult {
        let ensemble = try_arg!(arg_str(args, "ensemble"));
        let path = try_arg!(arg_str(args, "path"));
        run_zk(&ensemble, ZkCommand::Get(path))
    }
}

struct ZkCreate;
impl Tool for ZkCreate {
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: "zk_create", description: "Create a persistent ZooKeeper znode with optional data.",
            input_schema: zk_schema(&[("data", "Initial node data")], &["ensemble", "path"]) }
    }
    fn call(&self, _ctx: &Ctx, args: &Value) -> ToolResult {
        let ensemble = try_arg!(arg_str(args, "ensemble"));
        let path = try_arg!(arg_str(args, "path"));
        let data = arg_opt_str(args, "data").unwrap_or_default().into_bytes();
        run_zk(&ensemble, ZkCommand::Create(path, data))
    }
}

struct ZkSet;
impl Tool for ZkSet {
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: "zk_set", description: "Set the data of an existing ZooKeeper znode.",
            input_schema: zk_schema(&[("data", "New node data")], &["ensemble", "path", "data"]) }
    }
    fn call(&self, _ctx: &Ctx, args: &Value) -> ToolResult {
        let ensemble = try_arg!(arg_str(args, "ensemble"));
        let path = try_arg!(arg_str(args, "path"));
        let data = try_arg!(arg_str(args, "data")).into_bytes();
        run_zk(&ensemble, ZkCommand::Set(path, data))
    }
}

/// File name under `secrets/` for the password of `path`: every character outside
/// `[A-Za-z0-9_-]` (including `/` and `.`) becomes `_`.
fn secret_file_name(path: &str) -> String {
    path.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// The result of `zk_password`: names the secret file, never the password itself.
fn password_result(path: &str, file: &Path, _password: &str) -> ToolResult {
    let file = file.display().to_string();
    ToolResult::ok(
        format!("Stored a SCRAM-SHA-256 secret at {path}/authPassword. The generated password was written to {file} (mode 0600); read it from there."),
        Some(json!({"node": format!("{path}/authPassword"), "secret_file": file})),
    )
}

fn auth_node(path: &str) -> String {
    if path == "/" { "/authPassword".to_string() } else { format!("{path}/authPassword") }
}

/// Refuses (as a tool error) when `<path>/authPassword` already exists, so an existing password is never replaced unasked.
fn password_guard(store: &dyn crate::server::znode::ZnodeStore, path: &str) -> Result<(), ToolResult> {
    let node = auth_node(path);
    match store.get(&node) {
        Ok(None) => Ok(()),
        Ok(Some(_)) => Err(ToolResult::err(format!(
            "{node} already holds a password secret. Nothing was changed. Ask the user whether to replace it; \
             if they agree, call zk_password again with overwrite=true."))),
        Err(e) => Err(ToolResult::err(format!("cannot check {node}: {e}"))),
    }
}

struct ZkPassword;
impl Tool for ZkPassword {
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: "zk_password",
            description: "Generate a password, store its SCRAM-SHA-256 secret at <path>/authPassword, and save the plain password to a private file. Returns only the file path. \
                If <path>/authPassword already exists the call is refused: ask the user whether to replace the existing password, and only then call again with overwrite=true.",
            input_schema: zk_schema(&[("overwrite", "Replace an existing <path>/authPassword (default false); set only after the user agrees")], &["ensemble", "path"]) }
    }
    fn call(&self, ctx: &Ctx, args: &Value) -> ToolResult {
        let ensemble = try_arg!(arg_str(args, "ensemble"));
        let path = try_arg!(arg_str(args, "path"));
        if !path.starts_with('/') { return ToolResult::err("path must be an absolute znode path starting with `/`"); }
        let path = if path.len() > 1 { path.trim_end_matches('/').to_string() } else { path };
        let overwrite = arg_bool(args, "overwrite", false);
        if !overwrite {
            match crate::server::znode::ZkStore::connect(&ensemble) {
                Ok(store) => if let Err(r) = password_guard(&store, &path) { return r; },
                Err(e) => return ToolResult::err(e.to_string()),
            }
        }
        let password = random_password();
        let r = run_zk(&ensemble, ZkCommand::Password { path: path.clone(), password: password.clone() });
        if r.is_error { return r; }
        let file = ctx.home.secrets().join(secret_file_name(&path));
        match write_secret(&file, &password) {
            Ok(()) => password_result(&path, &file, &password),
            Err(e) => ToolResult::err(format!(
                "the secret was stored in ZooKeeper but saving the password to {} failed ({e}); run zk_password again", file.display())),
        }
    }
}

struct ListFeatures;
impl Tool for ListFeatures {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "arcus_list_features",
            description: "List the optional features that can be built into an ARCUS server for an edition. \
                Propose the recommended features to the user first; mention advanced (non-recommended) \
                features only when the user asks for them.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "edition": {"type": "string", "enum": ["public", "ee"], "description": "ARCUS edition"}
                },
                "required": ["edition"]
            }),
        }
    }
    fn call(&self, _ctx: &Ctx, args: &Value) -> ToolResult {
        let edition = match try_arg!(arg_str(args, "edition")).as_str() {
            "public" => Edition::Public,
            "ee" => Edition::Ee,
            other => return ToolResult::err(format!("invalid edition '{other}': use \"public\" or \"ee\"")),
        };
        let list: Vec<Value> = Feature::ALL.iter()
            .filter(|f| edition == Edition::Ee || !f.ee_only())
            .map(|f| json!({"name": f.key(), "label": f.label(), "recommended": f.recommended(), "ee_only": f.ee_only()}))
            .collect();
        let text = list.iter()
            .map(|v| format!("{} - {}{}", v["name"].as_str().unwrap_or(""), v["label"].as_str().unwrap_or(""),
                if v["recommended"] == true { " (recommended)" } else { "" }))
            .collect::<Vec<_>>().join("\n");
        ToolResult::ok(text, Some(json!({"features": list})))
    }
}

struct CheckPrerequisites;
impl Tool for CheckPrerequisites {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "arcus_check_prerequisites",
            description: "Check that the build tools needed to compile ARCUS are installed. Returns the missing \
                tools and an install command to show the user; it is never run automatically.",
            input_schema: json!({"type": "object", "properties": {}}),
        }
    }
    fn call(&self, _ctx: &Ctx, _args: &Value) -> ToolResult {
        let missing = missing_tools(&on_path);
        let hint = if missing.is_empty() { String::new() } else { install_hint(detect_os(), &missing) };
        let text = if missing.is_empty() {
            "All build prerequisites are installed.".to_string()
        } else {
            format!("Missing: {}\nSuggested install (run it yourself): {hint}", missing.join(", "))
        };
        ToolResult::ok(text, Some(json!({"missing": missing, "hint": hint})))
    }
}

struct JobStatusTool;
impl Tool for JobStatusTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "arcus_job_status",
            description: "Poll a background job (install, download, login) by id: state is running, succeeded or failed, \
                with the current step, a message on failure, and the log file path.",
            input_schema: json!({
                "type": "object",
                "properties": {"id": {"type": "string", "description": "Job id returned by the tool that started it"}},
                "required": ["id"]
            }),
        }
    }
    fn call(&self, ctx: &Ctx, args: &Value) -> ToolResult {
        let id = try_arg!(arg_str(args, "id"));
        match crate::server::jobs::status(&ctx.home, &id) {
            Ok(s) => {
                let text = format!("job {} is {:?} (step: {}){}", s.id, s.state, s.step,
                    if s.message.is_empty() { String::new() } else { format!(": {}", s.message) });
                ToolResult::ok(text, serde_json::to_value(&s).ok())
            }
            Err(e) => ToolResult::err(format!("arcus_job_status failed: {e}")),
        }
    }
}

const NO_LOGIN_HINT: &str = "EE access needs a GitHub login: call arcus_ee_login, show the code and URL to the user, then retry.";

struct ListVersions;
impl Tool for ListVersions {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "arcus_list_versions",
            description: "List installable ARCUS server versions for an edition, newest first. The EE edition needs a \
                GitHub login; if it reports that none exists, call arcus_ee_login.",
            input_schema: json!({
                "type": "object",
                "properties": {"edition": {"type": "string", "enum": ["public", "ee"], "description": "ARCUS edition"}},
                "required": ["edition"]
            }),
        }
    }
    fn call(&self, ctx: &Ctx, args: &Value) -> ToolResult {
        let edition = match try_arg!(arg_str(args, "edition")).as_str() {
            "public" => Edition::Public,
            "ee" => Edition::Ee,
            other => return ToolResult::err(format!("invalid edition '{other}': use \"public\" or \"ee\"")),
        };
        let listed = if edition == Edition::Ee {
            let tokens = find_tokens(find_gh(&ctx.home).as_deref());
            if tokens.is_empty() {
                return ToolResult::err(format!("No GitHub token found. {NO_LOGIN_HINT}"));
            }
            release::list_ee_releases(&tokens).map(|(l, _)| l)
                .map_err(|e| release::ee_failure("arcus_list_versions failed", &e))
        } else {
            release::list_releases(edition, None).map_err(|e| format!("arcus_list_versions failed: {e}"))
        };
        match listed {
            Ok(list) => {
                let versions: Vec<&str> = list.iter().map(|r| r.version.as_str()).collect();
                ToolResult::ok(versions.join("\n"), Some(json!({"versions": versions})))
            }
            Err(msg) => ToolResult::err(msg),
        }
    }
}

struct EeLogin;
impl Tool for EeLogin {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "arcus_ee_login",
            description: "Start a GitHub browser (device) login to get access to the private EE sources. Returns a one-time \
                code and URL; show both to the user, who approves in a browser. No token passes through this conversation. \
                On a Linux machine without a keyring, gh would store the token in plain text, so ask the user first and pass accept_plaintext_token: true.",
            input_schema: json!({"type": "object", "properties": {
                "accept_plaintext_token": {"type": "boolean", "description": "Allow gh to store the token in ~/.config/gh/hosts.yml (mode 600) when there is no keyring (default false)"}
            }}),
        }
    }
    fn call(&self, ctx: &Ctx, args: &Value) -> ToolResult {
        use crate::server::jobs::{self, JobKind, JobSpec};
        let has_dbus = std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some_and(|v| !v.is_empty());
        if plaintext_consent_needed(std::env::consts::OS, has_dbus, on_path("secret-tool"), arg_bool(args, "accept_plaintext_token", false)) {
            return plaintext_consent_error();
        }
        // The job downloads gh itself when it is missing, so this call never blocks on that download.
        let id = match jobs::submit(&ctx.home, JobSpec { kind: JobKind::EeLogin, params: json!({}) }) {
            Ok(id) => id,
            Err(e) => return ToolResult::err(format!("failed to start the login job: {e}")),
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(s) = jobs::status(&ctx.home, &id)
                && let Some(r) = login_reply(&id, &s, false) { return r; }
            if std::time::Instant::now() >= deadline { break; }
            std::thread::sleep(Duration::from_millis(250));
        }
        match jobs::status(&ctx.home, &id) {
            Ok(s) => login_reply(&id, &s, true).unwrap_or_else(|| ToolResult::err(format!("login job {id}: no status"))),
            Err(e) => ToolResult::err(format!("login job {id}: {e}")),
        }
    }
}

/// True on Linux without a keyring (no D-Bus session and no `secret-tool`) when the user has not consented to a plain-text token.
fn plaintext_consent_needed(os: &str, has_dbus: bool, has_secret_tool: bool, accepted: bool) -> bool {
    os == "linux" && !has_dbus && !has_secret_tool && !accepted
}

fn plaintext_consent_error() -> ToolResult {
    ToolResult::err("this machine has no keyring, so gh will store the GitHub token in plain text in ~/.config/gh/hosts.yml (mode 600). \
Ask the user whether that is acceptable; only if they agree, call arcus_ee_login again with accept_plaintext_token: true.")
}

/// Reply for the login job's current status: the code once published, a failure, or (`timed_out`) a poll instruction.
/// `None` while the job is still running and there is nothing to report yet.
fn login_reply(id: &str, s: &crate::server::jobs::JobStatus, timed_out: bool) -> Option<ToolResult> {
    use crate::server::jobs::JobState;
    if let Some(code) = s.result.as_ref().and_then(|r| r["code"].as_str()) {
        let url = s.result.as_ref().and_then(|r| r["url"].as_str()).unwrap_or(release::DEVICE_URL);
        return Some(ToolResult::ok(
            format!("Show this to the user: open {url} and enter the one-time code {code}. \
                Then poll arcus_job_status with id {id} until it is succeeded."),
            Some(json!({"job_id": id, "code": code, "url": url}))));
    }
    if s.state != JobState::Running {
        let tail = crate::server::jobs::tail(&s.log, 20).unwrap_or_default();
        return Some(ToolResult::err(format!("login job {id} {:?}: {}\nLog tail:\n{tail}", s.state, s.message)));
    }
    timed_out.then(|| ToolResult::ok(
        format!("The login job {id} is still preparing (step: {}), e.g. downloading the GitHub CLI. \
            Poll arcus_job_status with id {id}: as soon as its result contains `code` and `url`, show both to the user \
            (do not call arcus_ee_login again while the job is running); keep polling until it is succeeded. \
            If it fails, read the message and call arcus_ee_login again.", s.step),
        Some(json!({"job_id": id, "state": "running", "step": s.step}))))
}

struct Install;
impl Tool for Install {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "arcus_install",
            description: "Build and install an ARCUS server from source as a background job. Returns job_id and build_key; \
                poll arcus_job_status until it is succeeded (the build can take several minutes). Missing build tools are \
                reported with an install command to show the user. EE needs a GitHub login (arcus_ee_login). Arguments with recommended values (version): \
                propose them to the user and confirm before calling.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "edition": {"type": "string", "enum": ["public", "ee"], "description": "ARCUS edition"},
                    "version": {"type": "string", "description": "Version to build (recommended: the newest; see arcus_list_versions)"},
                    "features": {"type": "array", "items": {"type": "string"},
                        "description": "Feature names from arcus_list_features"}
                },
                "required": ["edition", "features"]
            }),
        }
    }
    fn call(&self, ctx: &Ctx, args: &Value) -> ToolResult {
        use crate::server::features::{build_key, validate};
        use crate::server::jobs::{self, JobKind, JobSpec};
        use std::collections::BTreeSet;
        let edition = match try_arg!(arg_str(args, "edition")).as_str() {
            "public" => Edition::Public,
            "ee" => Edition::Ee,
            other => return ToolResult::err(format!("invalid edition '{other}': use \"public\" or \"ee\"")),
        };
        let mut features = BTreeSet::new();
        for v in args.get("features").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]) {
            match serde_json::from_value::<Feature>(v.clone()) {
                Ok(f) => { features.insert(f); }
                Err(_) => return ToolResult::err(format!("unknown feature {v}; see arcus_list_features")),
            }
        }
        if let Err(e) = validate(edition, &features) { return ToolResult::err(e); }
        let missing = missing_tools(&on_path);
        if !missing.is_empty() {
            return ToolResult::err(format!("Missing build tools: {}\nAsk the user to run: {}",
                missing.join(", "), install_hint(detect_os(), &missing)));
        }
        let releases = if edition == Edition::Ee {
            let tokens = find_tokens(find_gh(&ctx.home).as_deref());
            if tokens.is_empty() {
                return ToolResult::err(format!("No GitHub token found for the EE edition. {NO_LOGIN_HINT}"));
            }
            match release::list_ee_releases(&tokens) {
                Ok((l, _)) => l,
                Err(e) => return ToolResult::err(release::ee_failure("arcus_install failed to list versions", &e)),
            }
        } else {
            match release::list_releases(edition, None) {
                Ok(l) => l,
                Err(e) => return ToolResult::err(format!("arcus_install failed to list versions: {e}")),
            }
        };
        let version = match arg_opt_str(args, "version") {
            Some(v) => v,
            None => match releases.first() {
                Some(r) => r.version.clone(),
                None => return ToolResult::err("no releases found"),
            },
        };
        if !releases.iter().any(|r| r.version == version) {
            return ToolResult::err(format!("version {version} not found; see arcus_list_versions"));
        }
        let key = build_key(edition, &version, &features);
        let params = json!({
            "edition": if edition == Edition::Ee { "ee" } else { "public" },
            "version": version,
            "features": features,
            "build_key": key,
        });
        match jobs::submit(&ctx.home, JobSpec { kind: JobKind::Install, params }) {
            Ok(id) => ToolResult::ok(
                format!("Started build {key} as job {id}. Poll arcus_job_status with this id until it is succeeded."),
                Some(json!({"job_id": id, "build_key": key}))),
            Err(e) => ToolResult::err(format!("failed to start the install job: {e}")),
        }
    }
}

struct ZkStart;
impl Tool for ZkStart {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "arcus_zk_start",
            description: "Start a local ZooKeeper ensemble (1 to 7 nodes, one process per node) for development. Arguments with \
                recommended values: propose them to the user and confirm before calling (count, ports, version). If ZooKeeper \
                (or Java) is not downloaded yet, this submits a download job and returns job_id: poll arcus_job_status until it is \
                succeeded, then call this again. Without any Java installed, set download_jre=true to also download a Temurin 17 JRE. \
                Returns the ensemble connect string. If a local ensemble is already running it is returned unchanged.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "count": {"type": "integer", "minimum": 1, "maximum": 7, "description": "Number of ZooKeeper nodes (recommended 3; 1 = standalone)"},
                    "client_ports": {"type": "array", "items": {"type": "integer"}, "description": "Client port per node, length = count (recommended 2181, 2182, ... consecutive)"},
                    "peer_ports": {"type": "array", "items": {"type": "integer"}, "description": "Quorum peer port per node, length = count (recommended 2888, 2889, ... consecutive)"},
                    "election_ports": {"type": "array", "items": {"type": "integer"}, "description": "Leader election port per node, length = count (recommended 3888, 3889, ... consecutive)"},
                    "base_client_port": {"type": "integer", "description": "First client port when client_ports is omitted (recommended 2181)"},
                    "base_peer_port": {"type": "integer", "description": "First peer port when peer_ports is omitted (recommended 2888)"},
                    "base_election_port": {"type": "integer", "description": "First election port when election_ports is omitted (recommended 3888)"},
                    "members": {"type": "array", "description": "Ensemble spread over several machines: the full member list [{id, address, client_port, peer_port, election_port}] (normally set by arcus_cluster_zk_start, not by hand)",
                        "items": {"type": "object"}},
                    "my_ids": {"type": "array", "items": {"type": "integer"}, "description": "With members: ids of the members to run on this machine"},
                    "version": {"type": "string", "description": "ZooKeeper version (recommended 3.8.6)"},
                    "download_jre": {"type": "boolean", "description": "Download a Temurin 17 JRE if no Java is found (default false)"}
                }
            }),
        }
    }
    fn call(&self, ctx: &Ctx, args: &Value) -> ToolResult {
        use crate::server::jobs::{self, JobKind, JobSpec};
        use crate::server::zookeeper as zk;
        let (nodes, my_ids) = match zk_members_from_args(args) {
            Ok(Some(m)) => (m.0, Some(m.1)),
            Ok(None) => match zk::nodes_from_args(args) { Ok(n) => (n, None), Err(e) => return ToolResult::err(e) },
            Err(e) => return ToolResult::err(e),
        };
        let version = arg_opt_str(args, "version").unwrap_or_else(|| zk::DEFAULT_ZK_VERSION.to_string());
        if !zk::valid_version(&version) { return ToolResult::err(format!("invalid ZooKeeper version `{version}`")); }
        let want_jre = arg_bool(args, "download_jre", false);
        let have_dist = ctx.home.zookeeper_dist(&version).join("lib").is_dir();
        let have_java = zk::find_java(&ctx.home).is_some();
        // A running ensemble is reported as it is; no download or Java is needed for that.
        if zk::read_ensemble(&ctx.home).is_none() || !have_dist || !have_java {
            if !have_java && !want_jre {
                return ToolResult::err("No Java 8+ found. Ask the user to install a JDK/JRE (e.g. `brew install openjdk` or \
                    `sudo apt-get install default-jre`), or call again with download_jre=true to download a Temurin 17 JRE.");
            }
            if !have_dist || !have_java {
                let params = json!({"version": version, "download_jre": !have_java});
                return match jobs::submit(&ctx.home, JobSpec { kind: JobKind::ZkDownload, params }) {
                    Ok(id) => ToolResult::ok(
                        format!("Started the ZooKeeper download as job {id}. Poll arcus_job_status until it is succeeded, then call arcus_zk_start again."),
                        Some(json!({"job_id": id}))),
                    Err(e) => ToolResult::err(format!("failed to start the download job: {e}")),
                };
            }
        }
        match crate::server::agent::zk_start_members(&ctx.home, &nodes, my_ids.as_deref(), &version) {
            Ok(st) => {
                let ensemble = st.ensemble.connect_string();
                let list: Vec<Value> = st.ensemble.local_nodes().into_iter().zip(&st.pids).map(|(n, pid)| json!({
                    "id": n.id, "client_port": n.client_port, "peer_port": n.peer_port, "election_port": n.election_port,
                    "pid": pid, "running": pid.is_some()})).collect();
                let note = if !st.already_running { String::new() } else if st.ensemble.nodes == nodes && st.ensemble.version == version && st.ensemble.local_ids == my_ids.clone().unwrap_or_default() {
                    " It was already running.".to_string()
                } else {
                    format!(" A local ensemble was already running with different settings (version {}), so nothing new was started; \
                        use {ensemble}, or call arcus_zk_stop first to start the requested one.", st.ensemble.version)
                };
                let down = list.iter().filter(|n| n["running"] == false).count();
                let note = if down > 0 { format!("{note} {down} node(s) are not running; call arcus_zk_stop and arcus_zk_start to restart the ensemble.") } else { note };
                ToolResult::ok(format!("ZooKeeper ensemble is running at {ensemble}.{note}"),
                    Some(json!({"ensemble": ensemble, "nodes": list, "already_running": st.already_running})))
            }
            Err(e) => ToolResult::err(format!("arcus_zk_start failed: {e}")),
        }
    }
}

/// `members` + `my_ids` of `arcus_zk_start` (an ensemble spread over several machines).
type ZkMembers = (Vec<crate::server::zookeeper::Node>, Vec<u32>);
fn zk_members_from_args(args: &Value) -> Result<Option<ZkMembers>, String> {
    let Some(m) = args.get("members").filter(|v| !v.is_null()) else { return Ok(None) };
    let members: Vec<crate::server::zookeeper::Node> = serde_json::from_value(m.clone()).map_err(|e| format!("bad `members`: {e}"))?;
    if members.is_empty() || members.len() > 7 { return Err("`members` must list 1 to 7 nodes".into()); }
    let ids: Vec<u32> = serde_json::from_value(args.get("my_ids").cloned().unwrap_or(Value::Null))
        .map_err(|_| "`my_ids` (array of node ids to run on this machine) is required with `members`".to_string())?;
    if ids.is_empty() { return Err("`my_ids` must not be empty".into()); }
    let mut seen = std::collections::BTreeSet::new();
    for n in &members { if !seen.insert(n.id) { return Err(format!("duplicate member id {}", n.id)); } }
    if let Some(b) = ids.iter().find(|i| !members.iter().any(|n| n.id == **i)) { return Err(format!("my_ids has {b}, which is not a member id")); }
    if members.len() > 1 && members.iter().any(|n| n.address.is_none()) { return Err("every member needs an `address` when several machines are used".into()); }
    Ok(Some((members, ids)))
}

struct ZkMode;
impl Tool for ZkMode {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "arcus_zk_mode",
            description: "Report the mode (leader, follower or standalone) of the ZooKeeper node listening on a client port of this computer; null when it is not serving.",
            input_schema: json!({"type": "object", "properties": {"port": {"type": "integer", "description": "Client port (recommended 2181)"}}, "required": ["port"]}),
        }
    }
    fn call(&self, _ctx: &Ctx, args: &Value) -> ToolResult {
        let Some(port) = arg_u64(args, "port").and_then(|p| u16::try_from(p).ok()).filter(|p| *p != 0) else {
            return ToolResult::err("missing or invalid argument `port` (1..=65535)");
        };
        let mode = crate::server::zookeeper::mode(port);
        ToolResult::ok(format!("ZooKeeper on port {port}: {}", mode.as_deref().unwrap_or("not serving")), Some(json!({"mode": mode})))
    }
}

struct ZkStop;
impl Tool for ZkStop {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "arcus_zk_stop",
            description: "Stop every node of the local ZooKeeper ensemble started by arcus_zk_start. Data is kept.",
            input_schema: json!({"type": "object", "properties": {}}),
        }
    }
    fn call(&self, ctx: &Ctx, _args: &Value) -> ToolResult {
        match crate::server::agent::zk_stop(&ctx.home) {
            Ok(n) => ToolResult::ok(format!("Stopped the local ZooKeeper ({n} process(es))."), Some(json!({"stopped": n}))),
            Err(e) => ToolResult::err(format!("arcus_zk_stop failed: {e}")),
        }
    }
}

/// Opens a znode store for an ensemble (a live ZooKeeper, or an in-memory store in tests).
type Connect<'a> = &'a dyn Fn(&str) -> std::io::Result<std::rc::Rc<dyn crate::server::znode::ZnodeStore>>;
type IpFor<'a> = &'a dyn Fn(&str) -> std::io::Result<std::net::IpAddr>;

const SECRET_PLACEHOLDER: &str = "<SCRAM secret>";

fn show_value(path: &str, v: &[u8]) -> String {
    if path.ends_with("/authPassword") { SECRET_PLACEHOLDER.to_string() } else { String::from_utf8_lossy(v).into_owned() }
}

fn setup_with(home: &crate::server::home::Home, args: &Value, connect: Connect, ip_for: IpFor) -> ToolResult {
    use crate::server::features::{build_key, validate};
    use crate::server::home::InstanceName;
    use crate::server::instance::{InstanceConfig, ReplConfig, SaslConfig, ZkTarget};
    use crate::server::znode::{apply, diff, plan, verify, Action};
    use std::collections::BTreeSet;

    let name = match InstanceName::parse(&try_arg!(arg_str(args, "name"))) { Ok(n) => n, Err(e) => return ToolResult::err(e) };
    let edition = match try_arg!(arg_str(args, "edition")).as_str() {
        "public" => Edition::Public,
        "ee" => Edition::Ee,
        other => return ToolResult::err(format!("invalid edition '{other}': use \"public\" or \"ee\"")),
    };
    let version = try_arg!(arg_str(args, "version"));
    let mut features = BTreeSet::new();
    for v in args.get("features").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]) {
        match serde_json::from_value::<Feature>(v.clone()) {
            Ok(f) => { features.insert(f); }
            Err(_) => return ToolResult::err(format!("unknown feature {v}; see arcus_list_features")),
        }
    }
    if let Err(e) = validate(edition, &features) { return ToolResult::err(e); }
    let port = match arg_u64(args, "port") {
        None => 11211,
        Some(p) if (1..=65535).contains(&p) => p as u16,
        Some(p) => return ToolResult::err(format!("invalid port {p}")),
    };
    let memory_mb = match arg_u64(args, "memory_mb") {
        None => 64,
        Some(m) if (1..=u32::MAX as u64).contains(&m) => m as u32,
        Some(m) => return ToolResult::err(format!("invalid memory_mb {m}")),
    };
    let do_apply = arg_bool(args, "apply", false);
    let overwrite: Vec<String> = args.get("overwrite").and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();

    /* Everything the user must decide is checked before anything is touched. */
    let has_zk = features.contains(&Feature::Zk);
    let (zk_target, service_code) = if has_zk {
        let z = try_arg!(arg_str(args, "zk"));
        let svc = try_arg!(arg_str(args, "service_code"));
        let target = if z == "local" {
            match crate::server::zookeeper::local_ensemble(home) {
                Some(ensemble) => ZkTarget { ensemble, local: true },
                None => return ToolResult::err("No local ZooKeeper ensemble is running. Call arcus_zk_start first (propose the node count and ports to the user), then retry."),
            }
        } else {
            ZkTarget { ensemble: z, local: false }
        };
        (Some(target), Some(svc))
    } else {
        if arg_opt_str(args, "zk").is_some() { return ToolResult::err("`zk` needs the `zk` feature; add it to features or drop the argument"); }
        (None, None)
    };
    let sasl = if features.contains(&Feature::Sasl) {
        let group = try_arg!(arg_str(args, "acl_group"));
        let user = arg_opt_str(args, "acl_user");
        if user.is_some() && !zk_target.as_ref().is_some_and(|z| z.local) {
            return ToolResult::err("acl_user can only be created on the local ZooKeeper (zk: \"local\"); an existing ensemble must already hold the user");
        }
        Some(SaslConfig { acl_group: group, user })
    } else { None };
    let repl_group = if features.contains(&Feature::Repl) { Some(try_arg!(arg_str(args, "repl_group"))) } else { None };

    let key = build_key(edition, &version, &features);
    let prefix = home.build_prefix(&key);
    if !crate::server::build::is_built(&prefix) {
        return ToolResult::err(format!("No complete build {key} found at {}. Call arcus_install with the same edition, version and features first.", prefix.display()));
    }

    let (store, ip) = match &zk_target {
        Some(z) => {
            let store = match connect(&z.ensemble) {
                Ok(s) => s,
                Err(e) => return ToolResult::err(format!("Cannot reach ZooKeeper at {}: {e}{}", z.ensemble,
                    if z.local { ". Call arcus_zk_start first." } else { "" })),
            };
            let ip = match ip_for(&z.ensemble) {
                Ok(ip) => ip,
                Err(e) => return ToolResult::err(format!("cannot determine this host's IP towards {}: {e}; set ARCUS_CACHE_PUBLIC_IP", z.ensemble)),
            };
            (Some(store), ip)
        }
        None => (None, std::net::IpAddr::from([127, 0, 0, 1])),
    };
    let repl = match repl_group {
        Some(group) => {
            let listen = match arg_opt_str(args, "repl_listen") {
                Some(l) => l,
                None => match port.checked_add(9000) {
                    Some(rp) => format!("{ip}:{rp}"),
                    None => return ToolResult::err("port + 9000 exceeds 65535; pass repl_listen"),
                },
            };
            Some(ReplConfig { group, listen })
        }
        None => None,
    };
    let cfg = InstanceConfig {
        name: name.as_str().to_string(), edition, version, features, build_key: key, port, memory_mb,
        zk: zk_target, service_code, sasl, repl,
    };

    /* An existing ensemble must already define the ACL group; we never create it there. */
    if let (Some(store), Some(z), Some(sc)) = (&store, &cfg.zk, &cfg.sasl)
        && !z.local {
            let g = format!("/arcus_acl/{}", sc.acl_group);
            match store.get(&g) {
                Ok(Some(_)) => {}
                Ok(None) => return ToolResult::err(format!("ACL group {g} does not exist on {}. Ask the user which ACL group to use (create it with arcusctl first).", z.ensemble)),
                Err(e) => return ToolResult::err(format!("cannot read {g}: {e}")),
            }
        }

    let inst_dir = home.instance(&name);
    let pw_path = inst_dir.join("sasl_password");
    let user_node = match (&cfg.zk, &cfg.sasl) {
        (Some(z), Some(sc)) if z.local => sc.user.as_ref().map(|u| format!("/arcus_acl/{}/{u}", sc.acl_group)),
        _ => None,
    };
    /* The secret in the plan: the stored one when this instance already owns a password file (so a rerun is a no-op),
       a fresh SCRAM secret on apply, or a placeholder in a dry run. The plain password is never output. */
    let mut fresh_password: Option<String> = None;
    let secret: Option<String> = match (&user_node, &store) {
        (Some(node), Some(store)) => {
            let existing = match store.get(&format!("{node}/authPassword")) {
                Ok(v) => v,
                Err(e) => return ToolResult::err(format!("cannot read {node}/authPassword: {e}")),
            };
            match (pw_path.is_file(), existing) {
                (true, Some(cur)) => Some(String::from_utf8_lossy(&cur).into_owned()),
                (have_file, _) if do_apply => {
                    let pw = if have_file {
                        match std::fs::read_to_string(&pw_path) { Ok(p) => p, Err(e) => return ToolResult::err(format!("cannot read {}: {e}", pw_path.display())) }
                    } else {
                        let p = random_password();
                        fresh_password = Some(p.clone());
                        p
                    };
                    Some(crate::zk::scram_sha256(&pw))
                }
                _ => Some(SECRET_PLACEHOLDER.to_string()),
            }
        }
        _ => None,
    };

    let diffs = match &store {
        Some(store) => match diff(&**store, plan(&cfg, ip, secret.as_deref())) {
            Ok(d) => d,
            Err(e) => return ToolResult::err(format!("cannot read the current znodes: {e}")),
        },
        None => Vec::new(),
    };
    let plan_json: Vec<Value> = diffs.iter().map(|(z, a)| {
        let mut e = json!({"path": z.path, "value": show_value(&z.path, &z.value)});
        match a {
            Action::Create => e["action"] = json!("create"),
            Action::Same => e["action"] = json!("same"),
            Action::Differs { current } => { e["action"] = json!("differs"); e["current"] = json!(show_value(&z.path, current)); }
        }
        e
    }).collect();
    let count = |act: &str| plan_json.iter().filter(|e| e["action"] == act).count();
    let summary = format!("{} to create, {} differ (kept unless listed in overwrite), {} already as planned",
        count("create"), count("differs"), count("same"));

    if !do_apply {
        let pw_note = user_node.as_ref().map(|_| format!(" The SASL password will be generated on apply and saved to {}.", pw_path.display())).unwrap_or_default();
        return ToolResult::ok(
            format!("Dry run, nothing was written: {summary}.{pw_note} Call arcus_setup again with apply=true to apply; list differing paths in overwrite to replace them.\n{}",
                serde_json::to_string_pretty(&plan_json).unwrap_or_default()),
            Some(json!({"applied": false, "plan": plan_json, "sasl_password_file": user_node.as_ref().map(|_| pw_path.display().to_string())})));
    }

    /* Save a freshly generated password only if its secret is actually going to be stored. */
    if let (Some(pw), Some(node)) = (&fresh_password, &user_node) {
        let auth = format!("{node}/authPassword");
        let stored = diffs.iter().any(|(z, a)| z.path == auth
            && (matches!(a, Action::Create) || (matches!(a, Action::Differs { .. }) && overwrite.contains(&auth))));
        if stored
            && let Err(e) = write_secret(&pw_path, pw) { return ToolResult::err(format!("cannot save {}: {e}", pw_path.display())); }
    }
    let written = match &store {
        Some(store) => match apply(&**store, &diffs, &overwrite) {
            Ok(w) => w,
            Err(e) => return ToolResult::err(format!("failed while writing znodes: {e}. Rerun arcus_setup with apply=true; existing znodes are kept.")),
        },
        None => Vec::new(),
    };
    let problems = store.as_ref().map(|s| verify(&**s, &cfg, ip)).unwrap_or_default();
    let toml_path = inst_dir.join("instance.toml");
    if let Err(e) = cfg.save(&toml_path) { return ToolResult::err(format!("znodes written but cannot save {}: {e}", toml_path.display())); }
    let kept: Vec<&str> = diffs.iter().filter(|(z, a)| matches!(a, Action::Differs { .. }) && !overwrite.contains(&z.path)).map(|(z, _)| z.path.as_str()).collect();
    let pw_file = (user_node.is_some() && pw_path.is_file()).then(|| pw_path.display().to_string());
    let mut text = format!("Applied instance '{}': wrote {} znodes, saved {}.", cfg.name, written.len(), toml_path.display());
    if !kept.is_empty() { text.push_str(&format!(" Kept differing znodes: {}.", kept.join(", "))); }
    if let Some(f) = &pw_file { text.push_str(&format!(" The SASL password is in {f} (mode 600); do not print it.")); }
    if !problems.is_empty() { text.push_str(&format!("\nProblems:\n- {}", problems.join("\n- "))); }
    let structured = json!({
        "applied": true, "written": written, "problems": problems, "kept_differing": kept,
        "instance_toml": toml_path.display().to_string(), "sasl_password_file": pw_file, "plan": plan_json,
    });
    // Verification problems mean the cluster setup is not usable yet: report an error, keep the details.
    ToolResult { text, structured: Some(structured), is_error: !problems.is_empty() }
}

struct Setup;
impl Tool for Setup {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "arcus_setup",
            description: "Define an ARCUS server instance and create the ZooKeeper znodes it needs to join a cluster. \
                Needs an existing build (arcus_install) and a reachable ZooKeeper (arcus_zk_start for local). Without apply it \
                only returns the plan (create/same/differs per znode) and writes nothing; call with apply=true after the user \
                agrees. With a local ZooKeeper and acl_user it creates the SASL user with a generated password saved to \
                instances/<name>/sasl_password (never shown). Ask the user for any missing name, service_code, acl_group or repl_group (no defaults). Arguments with recommended \
                values (port, memory_mb, repl_listen): propose them to the user and confirm before calling.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "Instance name, [A-Za-z0-9_-]{1,64}"},
                    "edition": {"type": "string", "enum": ["public", "ee"]},
                    "version": {"type": "string", "description": "Version of the existing build"},
                    "features": {"type": "array", "items": {"type": "string"}, "description": "Same features as the build"},
                    "port": {"type": "integer", "description": "Cache port (recommended 11211)"},
                    "memory_mb": {"type": "integer", "description": "Cache memory in MB (recommended 64)"},
                    "zk": {"type": "string", "description": "\"local\" (the ensemble started by arcus_zk_start) or an ensemble address; required with the zk feature"},
                    "service_code": {"type": "string", "description": "Service code (cloud name); required with zk"},
                    "acl_group": {"type": "string", "description": "ACL group; required with sasl"},
                    "acl_user": {"type": "string", "description": "SASL user to create (local ZooKeeper only)"},
                    "repl_group": {"type": "string", "description": "Replication group; required with repl"},
                    "repl_listen": {"type": "string", "description": "Replication listen ip:port (recommended <local ip>:<port+9000>)"},
                    "apply": {"type": "boolean", "description": "Write the changes (default false: plan only)"},
                    "overwrite": {"type": "array", "items": {"type": "string"}, "description": "Znode paths whose differing value may be replaced"}
                },
                "required": ["name", "edition", "version", "features"]
            }),
        }
    }
    fn call(&self, ctx: &Ctx, args: &Value) -> ToolResult {
        setup_with(&ctx.home, args, &|e| Ok(std::rc::Rc::new(crate::server::znode::ZkStore::connect(e)?)), &crate::server::instance::local_ip_for)
    }
}

fn parse_name(args: &Value) -> Result<crate::server::home::InstanceName, ToolResult> {
    let n = arg_str(args, "name")?;
    crate::server::home::InstanceName::parse(&n).map_err(ToolResult::err)
}

struct Start;
impl Tool for Start {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "arcus_start",
            description: "Start a cache server instance created by arcus_setup. It runs under a detached supervisor that records \
                how the server exits; returns once the server answers `version`.",
            input_schema: json!({"type": "object", "properties": {"name": {"type": "string", "description": "Instance name"}}, "required": ["name"]}),
        }
    }
    fn call(&self, ctx: &Ctx, args: &Value) -> ToolResult {
        let name = try_arg!(parse_name(args));
        match crate::server::agent::start(&ctx.home, &name) {
            Ok(pid) => ToolResult::ok(format!("Instance `{}` is running (pid {pid}).", name.as_str()),
                Some(json!({"name": name.as_str(), "pid": pid}))),
            Err(e) => ToolResult::err(format!("arcus_start failed: {e}")),
        }
    }
}

struct Stop;
impl Tool for Stop {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "arcus_stop",
            description: "Stop a cache server instance with SIGTERM (waits up to 10 s). If it is still running, ask the user \
                before calling again with force=true, which sends SIGKILL.",
            input_schema: json!({"type": "object", "properties": {
                "name": {"type": "string", "description": "Instance name"},
                "force": {"type": "boolean", "description": "Send SIGKILL if SIGTERM did not stop it (default false); only after the user agrees"}
            }, "required": ["name"]}),
        }
    }
    fn call(&self, ctx: &Ctx, args: &Value) -> ToolResult {
        use crate::server::supervise::StopOutcome::*;
        let name = try_arg!(parse_name(args));
        match crate::server::agent::stop(&ctx.home, &name, arg_bool(args, "force", false)) {
            Ok(o) => {
                let outcome = match o { Stopped => "stopped", StillRunning => "still_running", Killed => "killed", NotRunning => "not_running" };
                let text = match o {
                    Stopped => format!("Instance `{}` stopped.", name.as_str()),
                    Killed => format!("Instance `{}` was killed with SIGKILL.", name.as_str()),
                    NotRunning => format!("Instance `{}` is not running.", name.as_str()),
                    StillRunning => format!("Instance `{}` did not stop within 10 s of SIGTERM. Ask the user whether to force it; \
                        only if they agree, call arcus_stop again with force=true (SIGKILL).", name.as_str()),
                };
                ToolResult::ok(text, Some(json!({"outcome": outcome})))
            }
            Err(e) => ToolResult::err(format!("arcus_stop failed: {e}")),
        }
    }
}

struct StatusTool;
impl Tool for StatusTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "arcus_status",
            description: "Show whether cache server instances are running and responding, with the last recorded exit \
                (exit code or signal). Omit name to list all instances.",
            input_schema: json!({"type": "object", "properties": {"name": {"type": "string", "description": "Instance name; omit to list all instances"}}}),
        }
    }
    fn call(&self, ctx: &Ctx, args: &Value) -> ToolResult {
        let name = match arg_opt_str(args, "name") {
            Some(n) => Some(try_arg!(crate::server::home::InstanceName::parse(&n).map_err(ToolResult::err))),
            None => None,
        };
        match crate::server::agent::status(&ctx.home, name.as_ref()) {
            Ok(list) => {
                let text = if list.is_empty() { "No instances.".to_string() } else {
                    list.iter().map(|s| format!("{}: {}, port {}, responding={}{}", s.name,
                        if s.running { format!("running (pid {})", s.pid.unwrap_or(0)) } else { "stopped".into() },
                        s.port, s.responding,
                        s.last_exit.as_ref().map(|e| format!(", last exit {} code={:?} signal={:?}{}", e.time, e.code, e.signal,
                            e.note.as_ref().map(|n| format!(" ({n})")).unwrap_or_default())).unwrap_or_default()))
                        .collect::<Vec<_>>().join("\n")
                };
                ToolResult::ok(text, Some(json!({"instances": list})))
            }
            Err(e) => ToolResult::err(format!("arcus_status failed: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::Ctx;
    use crate::server::home::Home;
    use serde_json::json;
    use std::path::Path;

    fn ctx() -> Ctx {
        let root = std::env::temp_dir().join(format!("arcus-tools-{}", std::process::id()));
        Ctx { home: Home::new(Some(root)).unwrap() }
    }

    #[test]
    fn password_result_has_path_not_password() {
        let r = password_result("/arcus/cloud", Path::new("/h/secrets/_arcus_cloud"), "S3cretPassw0rd");
        assert!(!r.is_error);
        assert!(r.text.contains("/h/secrets/_arcus_cloud"));
        let all = format!("{} {:?}", r.text, r.structured);
        assert!(!all.contains("S3cretPassw0rd"));
    }

    #[test]
    fn secret_file_name_maps_slashes() {
        assert_eq!(secret_file_name("/arcus/cloud"), "_arcus_cloud");
        assert_eq!(secret_file_name("/"), "_");
    }

    #[test]
    fn zk_password_unreachable_is_error() {
        let t = registry().into_iter().find(|t| t.spec().name == "zk_password").unwrap();
        let r = t.call(&ctx(), &json!({"ensemble": "127.0.0.1:1", "path": "/x"}));
        assert!(r.is_error);
    }

    #[test]
    fn registry_has_no_delete_and_expected_tools() {
        let names: Vec<_> = registry().iter().map(|t| t.spec().name).collect();
        for n in ["memcached_command", "zk_ls", "zk_get", "zk_create", "zk_set", "zk_password", "arcus_zk_start", "arcus_zk_stop", "arcus_start", "arcus_stop", "arcus_status"] {
            assert!(names.contains(&n), "{n}");
        }
        assert!(!names.iter().any(|n| n.contains("delete")));
    }

    #[test]
    fn memcached_command_requires_command() {
        let t = registry().into_iter().find(|t| t.spec().name == "memcached_command").unwrap();
        assert!(t.call(&ctx(), &json!({})).is_error);
    }

    #[test]
    fn public_features_hide_ee_only() {
        let t = registry().into_iter().find(|t| t.spec().name == "arcus_list_features").unwrap();
        let r = t.call(&ctx(), &json!({"edition": "public"}));
        assert!(!r.is_error);
        let names: Vec<_> = r.structured.unwrap()["features"].as_array().unwrap().iter()
            .map(|v| v["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names, ["zk", "sasl", "persistence", "sticky"]);
        assert!(t.call(&ctx(), &json!({"edition": "x"})).is_error);
        assert!(t.call(&ctx(), &json!({})).is_error);
    }

    #[test]
    fn check_prerequisites_returns_missing_and_hint() {
        let t = registry().into_iter().find(|t| t.spec().name == "arcus_check_prerequisites").unwrap();
        let r = t.call(&ctx(), &json!({}));
        assert!(!r.is_error);
        let s = r.structured.unwrap();
        assert!(s["missing"].is_array() && s["hint"].is_string());
    }

    #[test]
    fn list_versions_validates_edition() {
        let t = registry().into_iter().find(|t| t.spec().name == "arcus_list_versions").unwrap();
        assert!(t.call(&ctx(), &json!({})).is_error);
        assert!(t.call(&ctx(), &json!({"edition": "x"})).is_error);
    }

    #[test]
    fn install_rejects_invalid_features() {
        let c = ctx();
        let t = registry().into_iter().find(|t| t.spec().name == "arcus_install").unwrap();
        let r = t.call(&c, &json!({"edition": "public", "features": ["repl"]}));
        assert!(r.is_error);
        assert!(r.text.contains("EE"), "{}", r.text);
        assert!(t.call(&c, &json!({"edition": "public", "features": ["bogus"]})).is_error);
        assert!(!c.home.jobs().exists() || std::fs::read_dir(c.home.jobs()).unwrap().next().is_none());
    }

    #[test]
    fn idle_is_clamped() {
        assert_eq!(idle_of(&json!({})), Duration::from_millis(300));
        assert_eq!(idle_of(&json!({"idle_ms": 5})), Duration::from_millis(5));
        assert_eq!(idle_of(&json!({"idle_ms": u64::MAX / 4})), MAX_WAIT);
    }

    #[test]
    fn password_guard_refuses_existing_node() {
        use crate::server::znode::tests::MemStore;
        use crate::server::znode::ZnodeStore;
        let st = MemStore::default();
        st.create("/arcus_acl", b"").unwrap();
        st.create("/arcus_acl/grp", b"").unwrap();
        assert!(password_guard(&st, "/arcus_acl/grp").is_ok());
        st.create("/arcus_acl/grp/authPassword", b"SCRAM-SHA-256$secret").unwrap();
        let r = password_guard(&st, "/arcus_acl/grp").unwrap_err();
        assert!(r.is_error && r.text.contains("overwrite=true") && r.text.contains("Ask the user") && !r.text.contains("SCRAM"), "{}", r.text);
        assert_eq!(auth_node("/"), "/authPassword");
        let spec = ZkPassword.spec();
        assert_eq!(spec.input_schema["properties"]["overwrite"]["type"], "boolean");
    }

    fn job_status(state: crate::server::jobs::JobState, step: &str, result: Option<Value>) -> crate::server::jobs::JobStatus {
        crate::server::jobs::JobStatus {
            id: "ee_login-1".into(), kind: crate::server::jobs::JobKind::EeLogin, state, step: step.into(),
            message: "boom".into(), log: std::path::PathBuf::from("/nonexistent/job.log"), result, pid: None,
        }
    }

    #[test]
    fn ee_login_requires_plaintext_consent_without_keyring() {
        assert!(plaintext_consent_needed("linux", false, false, false));
        assert!(!plaintext_consent_needed("linux", false, false, true));
        assert!(!plaintext_consent_needed("linux", true, false, false));
        assert!(!plaintext_consent_needed("linux", false, true, false));
        assert!(!plaintext_consent_needed("macos", false, false, false));
        let r = plaintext_consent_error();
        assert!(r.is_error && r.text.contains("hosts.yml") && r.text.contains("accept_plaintext_token"), "{}", r.text);
    }

    #[test]
    fn login_reply_variants() {
        use crate::server::jobs::JobState::*;
        // still downloading gh: nothing yet, then a poll instruction on timeout
        let s = job_status(Running, "download gh", None);
        assert!(login_reply("j", &s, false).is_none());
        let r = login_reply("j", &s, true).unwrap();
        assert!(!r.is_error && r.text.contains("arcus_job_status") && r.text.contains("download gh"), "{}", r.text);
        assert_eq!(r.structured.unwrap()["job_id"], "j");
        // code published
        let s = job_status(Running, "waiting for browser approval", Some(json!({"code": "ABCD-1234", "url": "https://github.com/login/device"})));
        let r = login_reply("j", &s, false).unwrap();
        assert!(!r.is_error && r.text.contains("ABCD-1234"), "{}", r.text);
        assert_eq!(r.structured.unwrap()["code"], "ABCD-1234");
        // failure
        let r = login_reply("j", &job_status(Failed, "download gh", None), false).unwrap();
        assert!(r.is_error && r.text.contains("boom"), "{}", r.text);
    }

    #[test]
    fn ee_tools_registered() {
        let names: Vec<_> = registry().iter().map(|t| t.spec().name).collect();
        assert!(names.contains(&"arcus_list_versions") && names.contains(&"arcus_ee_login"));
    }

    #[test]
    fn job_status_tool_unknown_id_is_error() {
        let t = registry().into_iter().find(|t| t.spec().name == "arcus_job_status").unwrap();
        assert!(t.call(&ctx(), &json!({"id": "install-1"})).is_error);
        assert!(t.call(&ctx(), &json!({})).is_error);
    }

    #[test]
    fn schema_defaults_are_marked_recommended() {
        for tool in registry() {
            let spec = tool.spec();
            let Some(props) = spec.input_schema["properties"].as_object() else { continue };
            for (k, p) in props {
                let d = p["description"].as_str().unwrap_or("");
                if p["type"] != "boolean" && d.to_lowercase().contains("default") {
                    assert!(d.contains("recommended"), "{}.{k}: {d}", spec.name);
                }
            }
        }
        let with_defaults = ["memcached_command", "arcus_zk_start", "arcus_setup", "arcus_install"];
        for tool in registry().iter().filter(|t| with_defaults.contains(&t.spec().name)) {
            assert!(tool.spec().description.contains("propose them to the user and confirm before calling"), "{}", tool.spec().name);
        }
    }

    #[test]
    fn zk_start_rejects_bad_ports_before_touching_anything() {
        let c = ctx();
        let t = registry().into_iter().find(|t| t.spec().name == "arcus_zk_start").unwrap();
        let r = t.call(&c, &json!({"count": 2, "client_ports": [30001]}));
        assert!(r.is_error && r.text.contains("client_ports"), "{}", r.text);
        assert!(t.call(&c, &json!({"count": 9})).is_error);
    }

    mod setup_tests {
        use super::*;
        use crate::server::znode::tests::MemStore;
        use crate::server::znode::ZnodeStore;
        use std::rc::Rc;

        fn tmp_home(tag: &str) -> Home {
            let root = std::env::temp_dir().join(format!("arcus-setup-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            Home::new(Some(root)).unwrap()
        }
        fn fake_build(h: &Home, key: &str) {
            let bin = h.build_prefix(key).join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::write(bin.join("memcached"), "").unwrap();
            std::fs::write(h.build_prefix(key).join(".arcus-installed"), "").unwrap();
        }
        fn run(h: &Home, store: &Rc<MemStore>, args: Value) -> ToolResult {
            let s = store.clone();
            setup_with(h, &args, &move |_| Ok(s.clone() as Rc<dyn ZnodeStore>), &|_| Ok("10.0.0.1".parse().unwrap()))
        }
        fn args(extra: Value) -> Value {
            let mut a = json!({"name": "dev", "edition": "ee", "version": "0.12.1", "features": ["zk"],
                "zk": "10.0.0.1:2181", "service_code": "svc1"});
            for (k, v) in extra.as_object().unwrap() { a[k] = v.clone(); }
            a
        }

        #[test]
        fn missing_names_ask_user() {
            let h = tmp_home("missing");
            let r = run(&h, &Rc::new(MemStore::default()), json!({"name": "dev", "edition": "ee", "version": "1", "features": ["zk"], "zk": "10.0.0.1:2181"}));
            assert!(r.is_error && r.text.contains("service_code") && r.text.contains("ask the user"), "{}", r.text);
        }

        #[test]
        fn partial_build_is_not_a_build() {
            let h = tmp_home("partial");
            let bin = h.build_prefix("ee-0.12.1-zk").join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::write(bin.join("memcached"), "").unwrap(); // no .arcus-installed marker
            let r = run(&h, &Rc::new(MemStore::default()), args(json!({})));
            assert!(r.is_error && r.text.contains("arcus_install"), "{}", r.text);
        }

        #[test]
        fn verify_problems_are_an_error_with_details() {
            struct NoChildren(MemStore);
            impl ZnodeStore for NoChildren {
                fn get(&self, p: &str) -> std::io::Result<Option<Vec<u8>>> { self.0.get(p) }
                fn create(&self, p: &str, v: &[u8]) -> std::io::Result<()> { self.0.create(p, v) }
                fn set(&self, p: &str, v: &[u8]) -> std::io::Result<()> { self.0.set(p, v) }
                fn children(&self, _p: &str) -> std::io::Result<Vec<String>> { Ok(Vec::new()) }
            }
            let h = tmp_home("problems");
            fake_build(&h, "ee-0.12.1-zk");
            let st: Rc<dyn ZnodeStore> = Rc::new(NoChildren(MemStore::default()));
            let r = setup_with(&h, &args(json!({"apply": true})), &move |_| Ok(st.clone()), &|_| Ok("10.0.0.1".parse().unwrap()));
            assert!(r.is_error, "{}", r.text);
            assert!(r.text.contains("Problems:"));
            let sc = r.structured.unwrap();
            assert_eq!(sc["applied"], true);
            assert_eq!(sc["problems"].as_array().unwrap().len(), 1);
        }

        #[test]
        fn requires_existing_build() {
            let h = tmp_home("nobuild");
            let r = run(&h, &Rc::new(MemStore::default()), args(json!({})));
            assert!(r.is_error && r.text.contains("arcus_install"), "{}", r.text);
        }

        #[test]
        fn no_apply_writes_nothing() {
            let h = tmp_home("dry");
            fake_build(&h, "ee-0.12.1-zk");
            let st = Rc::new(MemStore::default());
            let r = run(&h, &st, args(json!({})));
            assert!(!r.is_error, "{}", r.text);
            assert!(st.0.borrow().is_empty());
            assert!(!h.root.join("instances").exists());
            let plan = r.structured.unwrap()["plan"].as_array().unwrap().clone();
            assert!(plan.iter().any(|e| e["path"] == "/arcus/cache_list/svc1" && e["action"] == "create"));
        }

        #[test]
        fn apply_writes_and_saves() {
            let h = tmp_home("apply");
            fake_build(&h, "ee-0.12.1-zk");
            let st = Rc::new(MemStore::default());
            let r = run(&h, &st, args(json!({"apply": true})));
            assert!(!r.is_error, "{}", r.text);
            assert!(st.get("/arcus/cache_server_mapping/10.0.0.1:11211/svc1").unwrap().is_some());
            assert!(h.root.join("instances/dev/instance.toml").is_file());
            assert_eq!(r.structured.unwrap()["problems"].as_array().unwrap().len(), 0);
        }

        #[test]
        fn local_sasl_user_password_stays_out_of_output() {
            let h = tmp_home("sasl");
            fake_build(&h, "ee-0.12.1-zk_sasl");
            crate::server::zookeeper::write_ensemble(&h, &crate::server::zookeeper::Ensemble { version: "3.8.6".into(),
                nodes: vec![crate::server::zookeeper::Node { id: 1, client_port: 22181, peer_port: 22888, election_port: 23888, address: None }], local_ids: vec![] }).unwrap();
            let st = Rc::new(MemStore::default());
            let a = args(json!({"features": ["zk", "sasl"], "zk": "local", "acl_group": "grp", "acl_user": "dev"}));
            let dry = run(&h, &st, a.clone());
            assert!(!dry.is_error, "{}", dry.text);
            assert!(!h.root.join("instances/dev/sasl_password").exists());
            let mut ap = a.clone(); ap["apply"] = json!(true);
            let r = run(&h, &st, ap.clone());
            assert!(!r.is_error, "{}", r.text);
            let pw_file = h.root.join("instances/dev/sasl_password");
            let pw = std::fs::read_to_string(&pw_file).unwrap();
            assert_eq!(pw.len(), 24);
            let all = format!("{} {:?} {} {:?}", dry.text, dry.structured, r.text, r.structured);
            assert!(!all.contains(&pw));
            assert!(all.contains("<SCRAM secret>") && all.contains("sasl_password"));
            let stored = String::from_utf8(st.get("/arcus_acl/grp/dev/authPassword").unwrap().unwrap()).unwrap();
            assert!(stored.starts_with("SCRAM-SHA-256$"), "{stored}");
            /* rerun: password reused and nothing differs */
            let r2 = run(&h, &st, ap);
            assert!(!r2.is_error, "{}", r2.text);
            assert_eq!(std::fs::read_to_string(&pw_file).unwrap(), pw);
            assert_eq!(st.get("/arcus_acl/grp/dev/authPassword").unwrap().unwrap(), stored.as_bytes());
            assert!(r2.structured.unwrap()["written"].as_array().unwrap().is_empty());
        }

        #[test]
        fn local_zk_resolves_to_the_ensemble_string() {
            use crate::server::zookeeper::{write_ensemble, Ensemble, Node};
            let h = tmp_home("localzk");
            fake_build(&h, "ee-0.12.1-zk");
            let seen = std::cell::RefCell::new(String::new());
            let st = Rc::new(MemStore::default());
            let go = |h: &Home| {
                let s = st.clone();
                setup_with(h, &args(json!({"zk": "local"})), &|e| { *seen.borrow_mut() = e.to_string(); Ok(s.clone() as Rc<dyn ZnodeStore>) }, &|_| Ok("10.0.0.1".parse().unwrap()))
            };
            let r = go(&h);
            assert!(r.is_error && r.text.contains("arcus_zk_start"), "{}", r.text);
            let nodes = (1..=3).map(|i| Node { id: i, client_port: 22180 + i as u16, peer_port: 22887 + i as u16, election_port: 23887 + i as u16, address: None }).collect();
            write_ensemble(&h, &Ensemble { version: "3.8.6".into(), nodes, local_ids: vec![] }).unwrap();
            let r = go(&h);
            assert!(!r.is_error, "{}", r.text);
            assert_eq!(*seen.borrow(), "127.0.0.1:22181,127.0.0.1:22182,127.0.0.1:22183");
        }

        #[test]
        fn existing_ensemble_needs_existing_acl_group() {
            let h = tmp_home("acl");
            fake_build(&h, "ee-0.12.1-zk_sasl");
            let st = Rc::new(MemStore::default());
            let a = args(json!({"features": ["zk", "sasl"], "acl_group": "grp", "apply": true}));
            let r = run(&h, &st, a.clone());
            assert!(r.is_error && r.text.contains("/arcus_acl/grp"), "{}", r.text);
            assert!(st.0.borrow().is_empty());
            st.create("/arcus_acl", b"").unwrap();
            st.create("/arcus_acl/grp", b"").unwrap();
            assert!(!run(&h, &st, a).is_error);
            assert_eq!(st.get("/arcus_acl_mapping/svc1").unwrap().unwrap(), b"grp");
        }

        #[test]
        fn acl_user_rejected_for_existing_ensemble() {
            let h = tmp_home("user");
            fake_build(&h, "ee-0.12.1-zk_sasl");
            let r = run(&h, &Rc::new(MemStore::default()), args(json!({"features": ["zk", "sasl"], "acl_group": "g", "acl_user": "u"})));
            assert!(r.is_error && r.text.contains("acl_user"), "{}", r.text);
        }
    }
}
