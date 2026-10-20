//! Route a tool call to another machine: `ssh <machine> ~/.arcus-cli/bin/arcus-cli --call <tool>`.

use crate::mcp::{Ctx, Tool, ToolResult, ToolSpec};
use crate::remote::machines::resolve;
use crate::remote::ssh::{HostKeyPolicy, Runner, Ssh, SshError, SystemRunner};
use serde_json::{json, Value};
use std::path::PathBuf;

const MAX_ERR_LINES: usize = 20;

/// Maps an SSH failure to a tool error the AI can act on.
pub fn ssh_error_result(e: SshError) -> ToolResult {
    match e {
        SshError::HostKeyUnknown { fingerprint } => ToolResult::err(format!(
            "unknown host key (fingerprint: {fingerprint}). Show this fingerprint to the user; only after the user agrees, \
call again with accept_host_key: true")),
        SshError::HostKeyChanged => ToolResult::err(
            "the host key has CHANGED (possible man-in-the-middle). Do not retry; tell the user to verify the machine"),
        SshError::AuthFailed { hint } => ToolResult::err(format!("authentication failed: {hint}")),
        SshError::Unreachable(m) => ToolResult::err(format!("machine unreachable: {m}")),
        SshError::Remote { status, stderr } => ToolResult::err(format!("remote command failed ({status}): {}", stderr.trim())),
    }
}

fn ssh_config_path() -> PathBuf {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".ssh/config")).unwrap_or_else(|| PathBuf::from(".ssh/config"))
}

fn first_lines(s: &str) -> String {
    s.lines().take(MAX_ERR_LINES).collect::<Vec<_>>().join("\n")
}

fn missing_binary_hint(machine: &str) -> ToolResult {
    ToolResult::err(format!(
        "arcus-cli is not installed on `{machine}` (~/.arcus-cli/bin/arcus-cli not found). Run arcus_host_install for this machine first"))
}

fn is_missing_binary(status: i32, text: &str) -> bool {
    status == 127 || text.contains("No such file") || text.contains("not found")
}

pub fn forward(ctx: &Ctx, tool: &str, args: &Value, machine: &str, runner: &dyn Runner) -> ToolResult {
    forward_with(ctx, tool, args, machine, runner, &ssh_config_path())
}

fn forward_with(ctx: &Ctx, tool: &str, args: &Value, machine: &str, runner: &dyn Runner, config: &std::path::Path) -> ToolResult {
    if !tool.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return ToolResult::err(format!("invalid tool name `{tool}`"));
    }
    let m = match resolve(&ctx.home, machine, config) { Ok(m) => m, Err(e) => return ToolResult::err(e) };
    let ssh = Ssh { home: &ctx.home, runner, machine: &m, policy: HostKeyPolicy::Strict };
    let cmd = format!("~/.arcus-cli/bin/arcus-cli --call {tool} --home ~/.arcus-cli");
    let stdin = args.to_string();
    let out = match ssh.exec(&cmd, stdin.as_bytes()) {
        Ok(o) => o,
        Err(SshError::Remote { status, stderr }) => {
            if is_missing_binary(status, &stderr) { return missing_binary_hint(machine); }
            return ToolResult::err(format!("remote command failed ({status}): {}", first_lines(stderr.trim())));
        }
        Err(e) => return ssh_error_result(e),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed = stdout.lines().find(|l| l.starts_with("{\"arcus_call\":"))
        .and_then(|l| serde_json::from_str::<Value>(l).ok());
    let Some(v) = parsed else {
        if is_missing_binary(out.status, &out.stderr) { return missing_binary_hint(machine); }
        return ToolResult::err(format!(
            "no result from `{machine}` for {tool} (exit {}):\n{}\n{}",
            out.status, first_lines(&stdout), first_lines(&out.stderr)));
    };
    let Some(mut res) = v.get("result").and_then(ToolResult::from_json) else {
        return ToolResult::err(format!("unparseable result from `{machine}` for {tool}"));
    };
    let mut s = match res.structured.take() { Some(Value::Object(o)) => Value::Object(o), Some(other) => json!({"value": other}), None => json!({}) };
    s["machine"] = json!(machine);
    res.structured = Some(s);
    let remote_tag = v.get("tag").and_then(Value::as_str).unwrap_or("");
    let local = crate::TAG;
    if remote_tag != local && remote_tag != "dev" && local != "dev" {
        res.text.push_str(&format!("\n(note: `{machine}` runs arcus-cli {remote_tag}, this computer runs {local})"));
    }
    res
}

/// Adds an optional `machine` argument to a tool and routes the call there when given.
pub struct Remote<T: Tool>(pub T);

impl<T: Tool> Tool for Remote<T> {
    fn spec(&self) -> ToolSpec {
        let mut s = self.0.spec();
        if let Some(props) = s.input_schema.get_mut("properties").and_then(Value::as_object_mut) {
            props.insert("machine".into(), json!({
                "type": "string",
                "description": "Machine name (see arcus_host_list); omit to run on this computer"}));
        }
        s
    }
    fn call(&self, ctx: &Ctx, args: &Value) -> ToolResult {
        let Some(machine) = args.get("machine").and_then(Value::as_str).filter(|m| !m.is_empty()) else {
            return self.0.call(ctx, args);
        };
        let mut rest = args.clone();
        if let Some(o) = rest.as_object_mut() { o.remove("machine"); }
        forward(ctx, self.0.spec().name, &rest, machine, &SystemRunner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::ssh::SshOutput;
    use crate::server::home::Home;
    use std::cell::RefCell;
    use std::io;

    struct Fake { reply: RefCell<Vec<SshOutput>>, stdin: RefCell<Vec<Vec<u8>>> }
    impl Runner for Fake {
        fn run(&self, _p: &str, _a: &[String], i: &[u8], _e: &[(String, String)]) -> io::Result<SshOutput> {
            self.stdin.borrow_mut().push(i.to_vec());
            Ok(self.reply.borrow_mut().remove(0))
        }
    }
    fn fake(status: i32, stdout: &str, stderr: &str) -> Fake {
        Fake { reply: RefCell::new(vec![SshOutput { status, stdout: stdout.as_bytes().to_vec(), stderr: stderr.into() }]), stdin: RefCell::new(vec![]) }
    }
    fn setup(tag: &str) -> (Ctx, PathBuf) {
        let d = std::env::temp_dir().join(format!("arcus-fw-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let cfg = d.join("config");
        std::fs::write(&cfg, "Host dev-1\n  HostName 10.0.0.1\n").unwrap();
        (Ctx { home: Home { root: d.join("home") } }, cfg)
    }

    struct Dummy;
    impl Tool for Dummy {
        fn spec(&self) -> ToolSpec {
            ToolSpec { name: "dummy", description: "d", input_schema: json!({"type": "object", "properties": {"x": {"type": "string"}}}) }
        }
        fn call(&self, _c: &Ctx, _a: &Value) -> ToolResult { ToolResult::ok("local", None) }
    }

    #[test]
    fn schema_gets_machine_property() {
        let s = Remote(Dummy).spec();
        assert_eq!(s.input_schema["properties"]["machine"]["type"], "string");
        assert!(s.input_schema["properties"]["x"].is_object());
    }

    #[test]
    fn forward_parses_result_and_tags_machine() {
        let (ctx, cfg) = setup("ok");
        let f = fake(0, "warning\n{\"arcus_call\":1,\"tag\":\"v1\",\"result\":{\"text\":\"ok\",\"structured\":{\"a\":1},\"is_error\":false}}\n", "");
        let r = forward_with(&ctx, "zk_ls", &json!({"path": "/"}), "dev-1", &f, &cfg);
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.starts_with("ok"));
        let s = r.structured.unwrap();
        assert!(s["a"] == 1 && s["machine"] == "dev-1");
        assert_eq!(f.stdin.borrow()[0], br#"{"path":"/"}"#.to_vec());
    }

    #[test]
    fn forward_missing_binary_hints_install() {
        let (ctx, cfg) = setup("missing");
        let f = fake(127, "", "bash: line 1: /home/u/.arcus-cli/bin/arcus-cli: No such file or directory");
        let r = forward_with(&ctx, "zk_ls", &json!({}), "dev-1", &f, &cfg);
        assert!(r.is_error && r.text.contains("arcus_host_install"), "{}", r.text);
    }

    #[test]
    fn forward_unparseable_output_is_error() {
        let (ctx, cfg) = setup("bad");
        let long: String = (0..50).map(|i| format!("line{i}\n")).collect();
        let f = fake(0, &long, "");
        let r = forward_with(&ctx, "zk_ls", &json!({}), "dev-1", &f, &cfg);
        assert!(r.is_error && r.text.contains("line19") && !r.text.contains("line20"), "{}", r.text);
    }

    #[test]
    fn no_machine_runs_locally() {
        let (ctx, _) = setup("local");
        let r = Remote(Dummy).call(&ctx, &json!({"x": "1"}));
        assert_eq!(r.text, "local");
    }
}
