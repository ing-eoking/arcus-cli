use super::{arg_bool, arg_opt_str, arg_str, arg_u64, Ctx, Tool, ToolResult, ToolSpec};
use crate::connect::collect_responses;
use crate::server::build::{detect_os, install_hint, missing_tools, on_path};
use crate::server::features::{Edition, Feature};
use crate::server::release::{self, find_gh, find_token};
use crate::server::secret::{random_password, write_secret};
use crate::zk::{zk_exec, ZkCommand};
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

const MAX_WAIT: Duration = Duration::from_secs(10);
const MAX_BYTES: usize = 1 << 20;

/// All tools exposed by `arcus-cli --mcp` (later tasks append here).
pub fn registry() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(MemcachedCommand),
        Box::new(ZkLs),
        Box::new(ZkGet),
        Box::new(ZkCreate),
        Box::new(ZkSet),
        Box::new(ZkPassword),
        Box::new(ListFeatures),
        Box::new(CheckPrerequisites),
        Box::new(JobStatusTool),
        Box::new(ListVersions),
        Box::new(EeLogin),
        Box::new(Install),
        Box::new(ZkStart),
        Box::new(ZkStop),
    ]
}

macro_rules! try_arg {
    ($e:expr) => { match $e { Ok(v) => v, Err(r) => return r } };
}

struct MemcachedCommand;
impl Tool for MemcachedCommand {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "memcached_command",
            description: "Send ASCII commands to an ARCUS/memcached server and return its response. \
                Multiple commands and data lines are separated by newlines, e.g. \"set a 0 0 1\\na\\nget a\".",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "host": {"type": "string", "description": "Server host (default 127.0.0.1)"},
                    "port": {"type": "integer", "description": "Server port (default 11211)"},
                    "unix_path": {"type": "string", "description": "Unix socket path; used instead of host/port"},
                    "command": {"type": "string", "description": "Commands separated by newlines, data lines included"},
                    "idle_ms": {"type": "integer", "description": "Wait this long without new output before returning (default 300)"}
                },
                "required": ["command"]
            }),
        }
    }
    fn call(&self, _ctx: &Ctx, args: &Value) -> ToolResult {
        let command = try_arg!(arg_str(args, "command"));
        let idle = Duration::from_millis(arg_u64(args, "idle_ms").unwrap_or(300));
        let (addr, unix) = match arg_opt_str(args, "unix_path") {
            Some(p) => (p, true),
            None => {
                let host = arg_opt_str(args, "host").unwrap_or_else(|| "127.0.0.1".into());
                let port = arg_u64(args, "port").unwrap_or(11211);
                (format!("{host}:{port}"), false)
            }
        };
        match collect_responses(&addr, unix, &command, idle, MAX_WAIT.max(idle), MAX_BYTES) {
            Ok(text) => ToolResult::ok(text, None),
            Err(e) => ToolResult::err(format!("memcached_command failed: {e}")),
        }
    }
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
    for (k, d) in extra { props[*k] = json!({"type": if *k == "recursive" {"boolean"} else {"string"}, "description": d}); }
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

struct ZkPassword;
impl Tool for ZkPassword {
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: "zk_password",
            description: "Generate a password, store its SCRAM-SHA-256 secret at <path>/authPassword, and save the plain password to a private file. Returns only the file path.",
            input_schema: zk_schema(&[], &["ensemble", "path"]) }
    }
    fn call(&self, ctx: &Ctx, args: &Value) -> ToolResult {
        let ensemble = try_arg!(arg_str(args, "ensemble"));
        let path = try_arg!(arg_str(args, "path"));
        if !path.starts_with('/') { return ToolResult::err("path must be an absolute znode path starting with `/`"); }
        let path = if path.len() > 1 { path.trim_end_matches('/').to_string() } else { path };
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
        let login_hint = "EE access needs a GitHub login: call arcus_ee_login, show the code and URL to the user, then retry.";
        let token = if edition == Edition::Ee {
            match find_token(find_gh(&ctx.home).as_deref()) {
                Some(t) => Some(t),
                None => return ToolResult::err(format!("No GitHub token found. {login_hint}")),
            }
        } else { None };
        match release::list_releases(edition, token.as_ref()) {
            Ok(list) => {
                let versions: Vec<&str> = list.iter().map(|r| r.version.as_str()).collect();
                ToolResult::ok(versions.join("\n"), Some(json!({"versions": versions})))
            }
            Err(e) if edition == Edition::Ee && (e.kind() == std::io::ErrorKind::PermissionDenied
                || e.kind() == std::io::ErrorKind::NotFound) =>
                ToolResult::err(format!("GitHub denied access to the EE releases ({e}). {login_hint}")),
            Err(e) => ToolResult::err(format!("arcus_list_versions failed: {e}")),
        }
    }
}

struct EeLogin;
impl Tool for EeLogin {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "arcus_ee_login",
            description: "Start a GitHub browser (device) login to get access to the private EE sources. Returns a one-time \
                code and URL; show both to the user, who approves in a browser. No token passes through this conversation.",
            input_schema: json!({"type": "object", "properties": {}}),
        }
    }
    fn call(&self, ctx: &Ctx, _args: &Value) -> ToolResult {
        use crate::server::jobs::{self, JobKind, JobSpec, JobState};
        // Downloading gh happens synchronously, before the job is submitted.
        if find_gh(&ctx.home).is_none() {
            if let Err(e) = release::download_gh(&ctx.home) {
                return ToolResult::err(format!("GitHub CLI (gh) is not installed and downloading it failed: {e}"));
            }
        }
        let id = match jobs::submit(&ctx.home, JobSpec { kind: JobKind::EeLogin, params: json!({}) }) {
            Ok(id) => id,
            Err(e) => return ToolResult::err(format!("failed to start the login job: {e}")),
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(s) = jobs::status(&ctx.home, &id) {
                let code = s.result.as_ref().and_then(|r| r["code"].as_str()).map(str::to_string);
                if let Some(code) = code {
                    let url = s.result.as_ref().and_then(|r| r["url"].as_str()).unwrap_or(release::DEVICE_URL).to_string();
                    return ToolResult::ok(
                        format!("Show this to the user: open {url} and enter the one-time code {code}. \
                            Then poll arcus_job_status with id {id} until it is succeeded."),
                        Some(json!({"job_id": id, "code": code, "url": url})));
                }
                if s.state != JobState::Running {
                    return ToolResult::err(format!("login job {id} {:?}: {}", s.state, s.message));
                }
            }
            if std::time::Instant::now() >= deadline { break; }
            std::thread::sleep(Duration::from_millis(250));
        }
        let tail = jobs::status(&ctx.home, &id).and_then(|s| jobs::tail(&s.log, 20)).unwrap_or_default();
        ToolResult::err(format!("no device code within 30 seconds (job {id}). Log tail:\n{tail}"))
    }
}

struct Install;
impl Tool for Install {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "arcus_install",
            description: "Build and install an ARCUS server from source as a background job. Returns job_id and build_key; \
                poll arcus_job_status until it is succeeded (the build can take several minutes). Missing build tools are \
                reported with an install command to show the user. EE needs a GitHub login (arcus_ee_login).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "edition": {"type": "string", "enum": ["public", "ee"], "description": "ARCUS edition"},
                    "version": {"type": "string", "description": "Version to build (default: newest)"},
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
        let token = if edition == Edition::Ee {
            match find_token(find_gh(&ctx.home).as_deref()) {
                Some(t) => Some(t),
                None => return ToolResult::err("No GitHub token found for the EE edition. Call arcus_ee_login, show the code and URL to the user, then retry."),
            }
        } else { None };
        let releases = match release::list_releases(edition, token.as_ref()) {
            Ok(l) => l,
            Err(e) => return ToolResult::err(format!("arcus_install failed to list versions: {e}")),
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
            description: "Start a local single-node ZooKeeper for development. If ZooKeeper (or Java) is not downloaded yet, \
                this submits a download job and returns job_id: poll arcus_job_status until it is succeeded, then call this again. \
                Without any Java installed, set download_jre=true to also download a Temurin 17 JRE.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "port": {"type": "integer", "description": "Client port (default 2181)"},
                    "version": {"type": "string", "description": "ZooKeeper version (default 3.8.x)"},
                    "download_jre": {"type": "boolean", "description": "Download a Temurin 17 JRE if no Java is found (default false)"}
                }
            }),
        }
    }
    fn call(&self, ctx: &Ctx, args: &Value) -> ToolResult {
        use crate::server::jobs::{self, JobKind, JobSpec};
        use crate::server::zookeeper as zk;
        let port = match arg_u64(args, "port") {
            None => 2181,
            Some(p) if (1..=65535).contains(&p) => p as u16,
            Some(p) => return ToolResult::err(format!("invalid port {p}")),
        };
        let version = arg_opt_str(args, "version").unwrap_or_else(|| zk::DEFAULT_ZK_VERSION.to_string());
        if !zk::valid_version(&version) { return ToolResult::err(format!("invalid ZooKeeper version `{version}`")); }
        let want_jre = arg_bool(args, "download_jre", false);
        let have_dist = ctx.home.zookeeper_dist(&version).join("lib").is_dir();
        let have_java = zk::find_java(&ctx.home).is_some();
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
        match zk::start(&ctx.home, port, &version) {
            Ok(pid) => {
                let ensemble = format!("127.0.0.1:{port}");
                ToolResult::ok(format!("ZooKeeper is running at {ensemble} (pid {pid})"), Some(json!({"ensemble": ensemble, "pid": pid})))
            }
            Err(e) => ToolResult::err(format!("arcus_zk_start failed: {e}")),
        }
    }
}

struct ZkStop;
impl Tool for ZkStop {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "arcus_zk_stop",
            description: "Stop the local ZooKeeper started by arcus_zk_start.",
            input_schema: json!({"type": "object", "properties": {}}),
        }
    }
    fn call(&self, ctx: &Ctx, _args: &Value) -> ToolResult {
        match crate::server::zookeeper::stop(&ctx.home) {
            Ok(()) => ToolResult::ok("Stopped the local ZooKeeper.", None),
            Err(e) => ToolResult::err(format!("arcus_zk_stop failed: {e}")),
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
        for n in ["memcached_command", "zk_ls", "zk_get", "zk_create", "zk_set", "zk_password", "arcus_zk_start", "arcus_zk_stop"] {
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
}
