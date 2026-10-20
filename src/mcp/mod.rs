//! Minimal synchronous MCP server over stdio: one JSON-RPC message per line.
pub mod tools;

use crate::server::home::Home;
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

const LATEST: &str = "2025-06-18";
const SUPPORTED: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

pub struct Ctx { pub home: Home }

pub struct ToolResult { pub text: String, pub structured: Option<Value>, pub is_error: bool }
impl ToolResult {
    pub fn ok(text: impl Into<String>, structured: Option<Value>) -> ToolResult {
        ToolResult { text: text.into(), structured, is_error: false }
    }
    pub fn err(text: impl Into<String>) -> ToolResult {
        ToolResult { text: text.into(), structured: None, is_error: true }
    }
}

pub struct ToolSpec { pub name: &'static str, pub description: &'static str, pub input_schema: Value }

pub trait Tool {
    fn spec(&self) -> ToolSpec;
    fn call(&self, ctx: &Ctx, args: &Value) -> ToolResult;
}

pub fn arg_str(args: &Value, key: &str) -> Result<String, ToolResult> {
    arg_opt_str(args, key).ok_or_else(|| ToolResult::err(format!("missing argument `{key}`; ask the user for it")))
}
pub fn arg_opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
}
pub fn arg_u64(args: &Value, key: &str) -> Option<u64> { args.get(key).and_then(Value::as_u64) }
pub fn arg_bool(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn reply(id: &Value, result: Value) -> Value { json!({"jsonrpc": "2.0", "id": id, "result": result}) }
fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn handle(msg: &Value, tools: &[Box<dyn Tool>], ctx: &Ctx) -> Option<Value> {
    let id = msg.get("id")?; // notifications get no response
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").unwrap_or(&Value::Null);
    Some(match method {
        "initialize" => {
            let v = params.get("protocolVersion").and_then(Value::as_str)
                .filter(|v| SUPPORTED.contains(v)).unwrap_or(LATEST);
            reply(id, json!({
                "protocolVersion": v,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "arcus-cli", "version": env!("CARGO_PKG_VERSION")},
            }))
        }
        "ping" => reply(id, json!({})),
        "tools/list" => {
            let list: Vec<Value> = tools.iter().map(|t| {
                let s = t.spec();
                json!({"name": s.name, "description": s.description, "inputSchema": s.input_schema})
            }).collect();
            reply(id, json!({"tools": list}))
        }
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let empty = json!({});
            let args = params.get("arguments").filter(|a| a.is_object()).unwrap_or(&empty);
            let res = match tools.iter().find(|t| t.spec().name == name) {
                Some(t) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| t.call(ctx, args)))
                    .unwrap_or_else(|_| ToolResult::err(format!("tool `{name}` panicked; the server is still running (see stderr)"))),
                None => ToolResult::err(format!("unknown tool `{name}`")),
            };
            let mut out = json!({"content": [{"type": "text", "text": res.text}], "isError": res.is_error});
            if let Some(s) = res.structured { out["structuredContent"] = s; }
            reply(id, out)
        }
        _ => error(id, -32601, &format!("method not found: {method}")),
    })
}

pub fn serve(input: impl BufRead, mut output: impl Write, tools: Vec<Box<dyn Tool>>, ctx: Ctx) -> io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() { continue; }
        let resp = match serde_json::from_str::<Value>(&line) {
            Err(e) => Some(error(&Value::Null, -32700, &format!("parse error: {e}"))),
            Ok(msg) if msg.is_object() => handle(&msg, &tools, &ctx),
            Ok(_) => Some(error(&Value::Null, -32600, "invalid request")),
        };
        if let Some(r) = resp {
            writeln!(output, "{r}")?;
            output.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::home::InstanceName;
    use serde_json::{json, Value};
    use std::io::Cursor;

    struct Echo;
    impl Tool for Echo {
        fn spec(&self) -> ToolSpec {
            ToolSpec { name: "echo", description: "Echo text", input_schema: json!({"type":"object"}) }
        }
        fn call(&self, _ctx: &Ctx, args: &Value) -> ToolResult {
            match arg_str(args, "text") { Ok(t) => ToolResult::ok(t, None), Err(e) => e }
        }
    }

    struct Boom;
    impl Tool for Boom {
        fn spec(&self) -> ToolSpec { ToolSpec { name: "boom", description: "Panics", input_schema: json!({"type":"object"}) } }
        fn call(&self, _ctx: &Ctx, _args: &Value) -> ToolResult { panic!("kaboom") }
    }

    fn raw(lines: &[&str]) -> String {
        let dir = std::env::temp_dir().join(format!("arcus-cli-mcp-test-{}", std::process::id()));
        let ctx = Ctx { home: crate::server::home::Home::new(Some(dir)).unwrap() };
        let mut out = Vec::new();
        let input = lines.join("\n") + "\n";
        serve(Cursor::new(input), &mut out, vec![Box::new(Echo), Box::new(Boom)], ctx).unwrap();
        String::from_utf8(out).unwrap()
    }
    fn run(lines: &[&str]) -> Vec<Value> {
        raw(lines).lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    #[test] fn initialize_and_list() {
        let out = run(&[r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
                        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(out[0]["result"]["serverInfo"]["name"], "arcus-cli");
        assert_eq!(out[1]["result"]["tools"][0]["name"], "echo");
        assert_eq!(out[1]["result"]["tools"][0]["inputSchema"]["type"], "object");
    }
    #[test] fn initialize_unknown_version_falls_back() {
        let out = run(&[r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#,
                        r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#]);
        assert_eq!(out[0]["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(out[1]["result"], json!({}));
    }
    #[test] fn call_and_errors() {
        let out = run(&[r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"echo","arguments":{"text":"hi"}}}"#,
                        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"nope","arguments":{}}}"#,
                        r#"{"jsonrpc":"2.0","id":5,"method":"bogus"}"#, "not json"]);
        assert_eq!(out[0]["result"]["content"][0]["text"], "hi");
        assert_eq!(out[0]["result"]["isError"], false);
        assert_eq!(out[1]["result"]["isError"], true);
        assert_eq!(out[2]["error"]["code"], -32601);
        assert_eq!(out[3]["error"]["code"], -32700);
        assert_eq!(out[3]["id"], Value::Null);
    }
    #[test] fn panicking_tool_returns_error_and_server_continues() {
        let out = run(&[r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"boom","arguments":{}}}"#,
                        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"echo","arguments":{"text":"still"}}}"#]);
        assert_eq!(out[0]["result"]["isError"], true);
        assert!(out[0]["result"]["content"][0]["text"].as_str().unwrap().contains("panicked"));
        assert_eq!(out[1]["result"]["content"][0]["text"], "still");
    }
    #[test] fn every_output_line_is_json() {
        let s = raw(&[r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#, "garbage",
                      r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"echo","arguments":{}}}"#]);
        assert!(s.ends_with('\n'));
        for l in s.lines() { serde_json::from_str::<Value>(l).unwrap(); }
        assert_eq!(s.lines().count(), 3);
    }
    #[test] fn instance_name_rules() {
        assert!(InstanceName::parse("cache-1_a").is_ok());
        for bad in ["", "../x", "a/b", "a b", ".", &"x".repeat(65)] { assert!(InstanceName::parse(bad).is_err(), "{bad}"); }
    }
    #[test] fn missing_arg_tells_ai_to_ask() {
        assert!(arg_str(&json!({}), "service_code").unwrap_err().text.contains("ask the user"));
    }
    #[test] fn home_layout() {
        let dir = std::env::temp_dir().join(format!("arcus-cli-home-test-{}", std::process::id()));
        let h = crate::server::home::Home::new(Some(dir.clone())).unwrap();
        assert!(dir.is_dir());
        assert_eq!(h.build_prefix("k"), dir.join("builds/k"));
        assert_eq!(h.instance(&InstanceName::parse("a").unwrap()), dir.join("instances/a"));
        assert_eq!(h.jobs(), dir.join("jobs"));
    }
}
