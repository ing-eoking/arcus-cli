use super::{arg_bool, arg_opt_str, arg_str, arg_u64, Ctx, Tool, ToolResult, ToolSpec};
use crate::connect::collect_responses;
use crate::server::build::{detect_os, install_hint, missing_tools, on_path};
use crate::server::features::{Edition, Feature};
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
        for n in ["memcached_command", "zk_ls", "zk_get", "zk_create", "zk_set", "zk_password"] {
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
    fn job_status_tool_unknown_id_is_error() {
        let t = registry().into_iter().find(|t| t.spec().name == "arcus_job_status").unwrap();
        assert!(t.call(&ctx(), &json!({"id": "install-1"})).is_error);
        assert!(t.call(&ctx(), &json!({})).is_error);
    }
}
