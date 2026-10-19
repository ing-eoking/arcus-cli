# MCP Server Install & Run Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `arcus-cli --mcp` is a stdio MCP server whose tools install, configure and run one ARCUS cache server (public or EE) plus an optional local ZooKeeper, and expose the existing memcached/ZooKeeper features to AI clients.

**Architecture:** A hand-written synchronous JSON-RPC 2.0 loop (`src/mcp/`) dispatches `tools/call` to tool functions. Tool logic lives in `src/server/` and is pure where possible (feature → configure flags, instance → server args, znode plan/diff, release parsing), with effects (HTTP, processes, ZooKeeper) behind small traits. Long work runs as detached `arcus-cli --job <id>` processes tracked by `arcus_job_status`; servers run under detached `arcus-cli --supervise <name>` processes that own memcached as a child. Existing REPL code is refactored only to take an output sink, so the REPLs behave as before.

**Tech Stack:** Rust 2024, clap 4, synchronous I/O only: `serde`/`serde_json`, `ureq` (rustls), `tar` + `flate2`, `sha2`, `toml`, `libc`; existing `zookeeper` 0.8, `ring`, `base64`.

**Spec:** `docs/superpowers/specs/2026-10-19-server-install-design.md`

## Global Constraints

- Existing usage (`--host`, `--port`, `--udp`, `--unix`, `--sasl`, `--zookeeper`, `--timeout`) behaves exactly as before. New flags: `--mcp`, `--home <dir>` (only with `--mcp`), and hidden internal `--job <id>`, `--supervise <name>`.
- In `--mcp` mode stdout carries only JSON-RPC messages, one per line. Diagnostics go to stderr; child process output goes to log files.
- Synchronous code only; no async runtime, no MCP SDK.
- Home: `~/.arcus-cli/` by default.
- Linux (RHEL9 aarch64, Ubuntu x86_64) and macOS (Intel, Apple Silicon), no root. Never call sudo or install into system paths.
- Values a person must choose (service code, ACL group, replication group) are required tool arguments with no default; a missing one returns `isError: true` with text telling the AI to ask the user.
- Secrets never pass through tool arguments or results: EE auth via `gh` web login; SASL user and `zk_password` passwords are generated and written to mode-600 files whose paths are returned.
- Writes needing confirmation: `arcus_setup` writes only with `apply: true`; existing differing znodes are overwritten only when listed in `overwrite`; `arcus_stop` sends SIGKILL only with `force: true`. No znode delete tool.
- Public versions offered: `>= 1.15.0` from `naver/arcus-memcached`, asset `arcus-memcached-<v>.tar.gz`. EE: `jam2in/arcus-memcached-EE`, tag `<v>-E`, asset `arcus-memcached-EE-<v>.tar.gz`, fetched via `GET /repos/jam2in/arcus-memcached-EE/releases/assets/<id>` with `Accept: application/octet-stream`.
- Recommended features: ZK integration, SASL, replication (EE only). Advanced: persistence, sticky item, migration (EE), squall/gust engine (EE).

## Review Focus

- Something prints to stdout in `--mcp` mode (a stray `println!` in reused REPL code, a child process inheriting stdout): the client connection breaks. Test that a full tool call writes only JSON lines (Task 1, Task 2).
- Corrupt or partial file in `downloads/` from an interrupted job: re-verified on reuse, deleted and re-fetched on mismatch (Task 4).
- Stale `server.pid` whose pid now belongs to another process: not reported as running, `arcus_start` not blocked (Task 10).
- Instance name with `/`, `..` or spaces: rejected before any path is built (Task 1).
- `gh` prints its device code on stderr in a format that changes or never prints it: `arcus_ee_login` returns an error with gh's raw output instead of hanging (Task 6).

---

## File Structure

| File | Responsibility |
|---|---|
| `src/main.rs` | New flags; dispatch `--mcp`, `--job`, `--supervise` before the REPLs |
| `src/mcp/mod.rs` | JSON-RPC stdio loop: `initialize`, `ping`, `tools/list`, `tools/call`, notifications |
| `src/mcp/tools.rs` | Tool registry: names, descriptions, JSON input schemas, dispatch |
| `src/server/home.rs` | `Home` paths, `InstanceName` validation |
| `src/server/features.rs` | `Edition`, `Feature`, configure args, build key |
| `src/server/fetch.rs` | HTTP download with checksum + cache, JSON GET |
| `src/server/jobs.rs` | Detached background jobs and their status files |
| `src/server/release.rs` | Release listing, token chain, `gh` discovery/download, web login |
| `src/server/build.rs` | Prerequisites, extract, deps/configure/make/install |
| `src/server/zookeeper.rs` | Local single-node ZooKeeper |
| `src/server/instance.rs` | `InstanceConfig` (instance.toml), server argument list, local IP |
| `src/server/znode.rs` | Znode plan, diff, apply, verify |
| `src/server/supervise.rs` | Supervisor process, start/stop/status, exit record |
| `src/server/secret.rs` | Random password generation and mode-600 secret files |
| `src/connect/*.rs` | Output sink for responses (stdout for REPL, buffer for MCP) |
| `src/zk/mod.rs` | `execute` writes to a sink; auth prompt only when interactive |

---

### Task 1: MCP stdio server skeleton, flags, home

**Files:**
- Modify: `src/main.rs`, `Cargo.toml` (add `serde`, `serde_json`)
- Create: `src/mcp/mod.rs`, `src/mcp/tools.rs`, `src/server/mod.rs`, `src/server/home.rs`

**Interfaces:**
- Produces:
  - `pub struct Home { pub root: PathBuf }`: `new(root: Option<PathBuf>) -> io::Result<Home>` (default `$HOME/.arcus-cli`, created), `downloads()`, `build_prefix(key: &str)` (`builds/<key>`), `zookeeper_dist(version: &str)`, `zk_dir()`, `jobs()`, `gh_dir()`, `secrets()`, `instance(&InstanceName)` (`instances/<name>`).
  - `pub struct InstanceName(String)`: `parse(&str) -> Result<InstanceName, String>` allowing `[A-Za-z0-9_-]{1,64}`; `as_str()`.
  - `pub struct ToolResult { pub text: String, pub structured: Option<serde_json::Value>, pub is_error: bool }` with `ok(text, structured)` and `err(text)`.
  - `pub struct ToolSpec { pub name: &'static str, pub description: &'static str, pub input_schema: serde_json::Value }`
  - `pub trait Tool { fn spec(&self) -> ToolSpec; fn call(&self, ctx: &Ctx, args: &serde_json::Value) -> ToolResult; }` and `pub struct Ctx { pub home: Home }`
  - `pub fn registry() -> Vec<Box<dyn Tool>>` in `tools.rs` (later tasks append).
  - `pub fn serve(input: impl BufRead, output: impl Write, tools: Vec<Box<dyn Tool>>, ctx: Ctx) -> io::Result<()>`: one JSON-RPC message per line. `initialize` → `{protocolVersion: <client's if in ["2025-06-18","2025-03-26","2024-11-05"] else "2025-06-18">, capabilities: {tools: {}}, serverInfo: {name: "arcus-cli", version: CARGO_PKG_VERSION}}`; `ping` → `{}`; `tools/list` → `{tools: [{name, description, inputSchema}]}`; `tools/call` → `{content: [{type: "text", text}], structuredContent?, isError}`; unknown tool → result with `isError: true`; unknown method → error `-32601`; parse error → `-32700` with `id: null`; messages without `id` get no response.
  - `pub fn arg_str(args: &Value, key: &str) -> Result<String, ToolResult>` (missing → `ToolResult::err("missing argument `<key>`; ask the user for it")`), `arg_opt_str`, `arg_u64`, `arg_bool(default)`.

- [ ] **Step 1: Write the failing tests** in `src/mcp/mod.rs` with a test `Echo` tool

```rust
fn run(lines: &[&str]) -> Vec<Value> { /* serve over Cursor input, collect output lines parsed as JSON */ }
#[test] fn initialize_and_list() {
    let out = run(&[r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
                    r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                    r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#]);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0]["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(out[1]["result"]["tools"][0]["name"], "echo");
}
#[test] fn call_and_errors() {
    let out = run(&[r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"echo","arguments":{"text":"hi"}}}"#,
                    r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"nope","arguments":{}}}"#,
                    r#"{"jsonrpc":"2.0","id":5,"method":"bogus"}"#, "not json"]);
    assert_eq!(out[0]["result"]["content"][0]["text"], "hi");
    assert_eq!(out[1]["result"]["isError"], true);
    assert_eq!(out[2]["error"]["code"], -32601);
    assert_eq!(out[3]["error"]["code"], -32700);
}
#[test] fn every_output_line_is_json() { /* all output lines parse as JSON and end with '\n' */ }
#[test] fn instance_name_rules() {
    assert!(InstanceName::parse("cache-1_a").is_ok());
    for bad in ["", "../x", "a/b", "a b", ".", &"x".repeat(65)] { assert!(InstanceName::parse(bad).is_err(), "{bad}"); }
}
#[test] fn missing_arg_tells_ai_to_ask() { assert!(arg_str(&json!({}), "service_code").unwrap_err().text.contains("ask the user")); }
```

In `main.rs` tests: `Args::try_parse_from(["arcus-cli","--host","h"])` still parses; `["arcus-cli","--mcp","--home","/x"]` parses; `["arcus-cli","--mcp","--zookeeper"]` fails (`conflicts_with_all`).

- [ ] **Step 2: Run** `cargo test mcp home main` — Expected: FAIL.
- [ ] **Step 3: Implement.** In `main.rs`, `--mcp` → `mcp::serve(stdin.lock(), stdout.lock(), tools::registry(), ctx)` and exit; `--job`/`--supervise` are `#[arg(hide = true)]` and print `not implemented yet` until Tasks 5/10.
- [ ] **Step 4: Run** `cargo test` — Expected: PASS including the existing 46 tests. Manual: `printf '{"jsonrpc":"2.0","id":1,"method":"tools/list"}\n' | cargo run -q -- --mcp` prints one JSON line.
- [ ] **Step 5: Commit** `INTERNAL: Add MCP stdio server skeleton`

---

### Task 2: Reuse memcached and ZooKeeper features as tools

**Files:**
- Modify: `src/connect/internal.rs`, `src/connect/mod.rs`, `src/connect/udp.rs`, `src/zk/mod.rs`, `src/mcp/tools.rs`
- Create: `src/server/secret.rs`

**Interfaces:**
- Consumes: `request::split`, `Transport`, `ZkClient`, `scram_sha256`
- Produces:
  - `pub type Sink = Arc<Mutex<Box<dyn Write + Send>>>` in `connect/mod.rs`; `TransportBuilder::sink(Sink)` (default: stdout). The reader thread and UDP printing write to the sink.
  - `pub fn collect_responses(addr: &str, unix: bool, input: &str, idle: Duration, max_wait: Duration, max_bytes: usize) -> io::Result<String>`: TCP/Unix only; builds a Transport with a buffer sink, sends every `request::split(input).requests`, returns when nothing new arrived for `idle` after the last byte (or `max_wait`/`max_bytes` hit, noting truncation). Pending (incomplete) input → error naming the first line.
  - `ZkClient::execute(&self, cmd: ZkCommand, out: &mut dyn Write, interactive: bool) -> bool`: all `println!`/`eprintln!` become writes to `out` (errors prefixed `ERROR: `); `with_auth_retry` prompts only when `interactive`, else returns `NoAuth`. `run_repl` passes stdout and `true`.
  - `pub fn zk_exec(ensemble: &str, cmd: ZkCommand) -> io::Result<String>` (connect, `execute(.., false)`, return text).
  - `pub fn random_password() -> String` (24 chars from `ring::rand`, alphanumeric) and `pub fn write_secret(path: &Path, value: &str) -> io::Result<()>` (create dirs, mode 0o600) in `secret.rs`.
  - Tools: `memcached_command {host?: "127.0.0.1", port?: 11211, unix_path?, command, idle_ms?: 300}`; `zk_ls {ensemble, path, recursive?}`, `zk_get {ensemble, path}`, `zk_create {ensemble, path, data?}`, `zk_set {ensemble, path, data}`, `zk_password {ensemble, path}` → generates a password, stores SCRAM secret at `<path>/authPassword`, writes the plain password to `secrets/<path with '/' → '_'>` and returns that file path only.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn collect_from_fake_server() {
    // TcpListener on 127.0.0.1:0; thread reads "get a\r\n" and replies "END\r\n"
    assert_eq!(collect_responses(&addr, false, "get a", ms(100), ms(2000), 1 << 20).unwrap(), "END\r\n");
}
#[test] fn collect_rejects_incomplete() { assert!(collect_responses(&addr, false, "set k 0 0 5", ms(100), ms(500), 1024).unwrap_err().to_string().contains("set k 0 0 5")); }
#[test] fn secret_file_mode() { /* write_secret then metadata().permissions().mode() & 0o777 == 0o600 */ }
#[test] fn password_tool_never_returns_password() { /* call zk_password against an unreachable ensemble → isError; and unit-test the result builder: text contains the file path, does not contain the generated password */ }
```

- [ ] **Step 2: Run** `cargo test collect secret password` — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test` — Expected: PASS. Manual: the REPL (`cargo run -- --port <p>`) and ZK REPL behave as before; via MCP, `memcached_command` with `"set a 0 0 1\na\nget a"` against a local server returns `STORED\r\nVALUE a 0 1\r\na\r\nEND\r\n`.
- [ ] **Step 5: Commit** `INTERNAL: Expose memcached and ZooKeeper commands as MCP tools`

---

### Task 3: Feature model and prerequisite tools

**Files:**
- Create: `src/server/features.rs`, `src/server/build.rs` (prerequisite part)
- Modify: `src/mcp/tools.rs`

**Interfaces:**
- Produces:
  - `#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)] #[serde(rename_all = "lowercase")] pub enum Edition { Public, Ee }`
  - `#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)] #[serde(rename_all = "lowercase")] pub enum Feature { Zk, Sasl, Repl, Persistence, Sticky, Migration, Squall, Gust }` — declaration order fixes argument and key order.
  - `impl Feature { pub fn recommended(self) -> bool; pub fn ee_only(self) -> bool; pub fn configure_flag(self, prefix: &Path) -> Vec<String>; pub fn key(self) -> &'static str; pub fn label(self) -> &'static str }`
  - `pub fn validate(edition: Edition, features: &BTreeSet<Feature>) -> Result<(), String>`: EE-only on Public → error; Sasl or Repl without Zk → error; Squall with Gust → error.
  - `pub fn configure_args(features: &BTreeSet<Feature>, prefix: &Path) -> Vec<String>`; `pub fn build_key(edition, version: &str, features) -> String` (`<public|ee>-<version>-<keys '_'-joined or "base">`).
  - `pub enum Os { Rhel, Debian, Mac, Other }`, `pub fn detect_os() -> Os`, `pub fn missing_tools(have: &dyn Fn(&str) -> bool) -> Vec<&'static str>` over `cc, make, libtool, perl, autoconf, automake` (`glibtool` on macOS), `pub fn install_hint(os: Os, missing: &[&str]) -> String` (Rhel `sudo dnf install gcc make libtool perl autoconf automake`; Debian `sudo apt-get install build-essential libtool perl autoconf automake`; Mac `brew install libtool autoconf automake`, plus `xcode-select --install` when `cc`/`make` missing) — shown to the user, never run.
  - Tools: `arcus_list_features {edition}` → structured `[{name, label, recommended, ee_only}]` (EE-only omitted for public); description tells the AI to propose recommended ones first and advanced ones only on request. `arcus_check_prerequisites {}` → `{missing: [...], hint}`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn configure_args_recommended_ee() {
    let f = BTreeSet::from([Feature::Zk, Feature::Sasl, Feature::Repl]);
    assert_eq!(configure_args(&f, Path::new("/h/b")), ["--prefix=/h/b","--with-libevent=/h/b",
        "--enable-zk-integration","--with-zookeeper=/h/b","--enable-sasl","--enable-replication"]);
}
#[test] fn advanced_flags() { /* Persistence→--enable-persistence, Sticky→--enable-sticky-item, Migration→--enable-migration, Squall→--enable-squall-engine, Gust→--enable-gust-engine */ }
#[test] fn validate_rules() { /* public+repl err; ee repl without zk err; squall+gust err; ee zk+sasl+repl ok */ }
#[test] fn build_key_stable() { assert_eq!(build_key(Edition::Ee, "0.12.1", &BTreeSet::from([Feature::Repl, Feature::Zk])), "ee-0.12.1-zk_repl"); }
#[test] fn hints() { assert!(install_hint(Os::Mac, &["cc"]).contains("xcode-select --install")); assert!(install_hint(Os::Rhel, &["libtool"]).contains("dnf install")); }
#[test] fn public_features_hide_ee_only() { /* arcus_list_features {edition:"public"} names == ["zk","sasl","persistence","sticky"] */ }
```

- [ ] **Step 2: Run** `cargo test features build` — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test` — Expected: PASS.
- [ ] **Step 5: Commit** `INTERNAL: Add server feature model and prerequisite check tools`

---

### Task 4: Verified, cached downloads

**Files:**
- Create: `src/server/fetch.rs`
- Modify: `Cargo.toml` (add `ureq`, `sha2`)

**Interfaces:**
- Produces:
  - `pub enum Checksum { Sha256(String), Sha512(String) }` (lowercase hex); `pub fn file_digest_matches(path: &Path, sum: &Checksum) -> io::Result<bool>`
  - `pub struct Request { pub url: String, pub headers: Vec<(String, String)> }`
  - `pub fn fetch_cached(req: &Request, dest: &Path, sum: Option<&Checksum>) -> io::Result<PathBuf>`: existing `dest` reused only if no sum or sum matches; mismatching `dest` is deleted and re-downloaded; downloads to `<dest>.part`, verifies, renames; error messages name the URL path but never header values.
  - `pub fn get_json(req: &Request) -> io::Result<Value>`; HTTP 401/403 → `PermissionDenied`, 404 → `NotFound`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn digest_known() { /* "abc" sha256 = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad */ }
#[test] fn valid_cache_reused_offline() { /* dest "abc" + matching sum, url http://127.0.0.1:9/x → Ok */ }
#[test] fn corrupt_cache_refetched() { /* dest "xyz" + sum of "abc", unreachable url → Err and dest removed */ }
#[test] fn mismatch_after_download_leaves_nothing() { /* local TcpListener serving "xyz" with sum of "abc" → Err, no dest, no .part */ }
```

- [ ] **Step 2: Run** `cargo test fetch` — Expected: FAIL.
- [ ] **Step 3: Implement** (stream to `.part` while hashing).
- [ ] **Step 4: Run** `cargo test fetch` — Expected: PASS.
- [ ] **Step 5: Commit** `INTERNAL: Add verified, cached downloads`

---

### Task 5: Background jobs

**Files:**
- Create: `src/server/jobs.rs`
- Modify: `src/main.rs` (`--job <id>`), `src/mcp/tools.rs`

**Interfaces:**
- Produces:
  - `#[derive(Serialize, Deserialize)] pub struct JobSpec { pub kind: JobKind, pub params: Value }`, `pub enum JobKind { Install, ZkDownload, EeLogin }` (handlers registered by Tasks 6–8).
  - `#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)] pub struct JobStatus { pub id: String, pub kind: JobKind, pub state: JobState /* running|succeeded|failed */, pub step: String, pub message: String, pub log: PathBuf, pub result: Option<Value> }`
  - `pub fn submit(home: &Home, spec: JobSpec) -> io::Result<String>`: id = `<kind>-<unix millis>`; writes `jobs/<id>/spec.json` and an initial `status.json`; spawns `current_exe() --job <id> --home <root>` with `process_group(0)`, stdin null, stdout/stderr → `jobs/<id>/job.log`.
  - `pub struct JobCtx { pub id: String, pub home: Home, pub log: PathBuf }` with `fn step(&self, name: &str)` (updates `status.json`).
  - `pub fn run_job(home: &Home, id: &str) -> io::Result<()>`: loads the spec, calls the handler `fn(&JobCtx, &Value) -> Result<Value, String>`, writes the final state; a panic or error → `failed` with the message and `tail(log, 20)`.
  - `pub fn status(home: &Home, id: &str) -> io::Result<JobStatus>`; a `running` job whose process is gone → reported `failed` with `job process exited unexpectedly`.
  - `pub fn tail(path: &Path, n: usize) -> io::Result<String>` (lives here; build uses it).
  - Tool: `arcus_job_status {id}`.

- [ ] **Step 1: Write the failing tests** (test handler kind gated by `#[cfg(test)]` running in-process via `run_job`)

```rust
#[test] fn job_success_records_result() { /* handler returns Ok(json!({"x":1})) → status.state == Succeeded, result == {"x":1} */ }
#[test] fn job_failure_records_tail() { /* handler writes 30 lines to ctx.log then Err("boom") → Failed, message contains "boom" and line 30, not line 5 */ }
#[test] fn dead_running_job_is_failed() { /* status.json state running with pid 999999 → status() reports Failed */ }
```

- [ ] **Step 2: Run** `cargo test jobs` — Expected: FAIL.
- [ ] **Step 3: Implement.** Record the job pid in `status.json` for the liveness check.
- [ ] **Step 4: Run** `cargo test jobs` — Expected: PASS.
- [ ] **Step 5: Commit** `INTERNAL: Add detached background jobs for long MCP tools`

---

### Task 6: Releases, gh web login, version listing

**Files:**
- Create: `src/server/release.rs`
- Modify: `src/mcp/tools.rs`, `src/server/jobs.rs` (register `EeLogin`)

**Interfaces:**
- Consumes: `fetch::{Request, get_json, fetch_cached, Checksum}`, `jobs::submit`, `Home::gh_dir`
- Produces:
  - `pub struct Release { pub version: String, pub asset_name: String, pub asset_api_url: String, pub sha256: Option<String> }`
  - `pub fn parse_releases(edition: Edition, json: &Value) -> Vec<Release>`: public keeps semver `>= 1.15.0` with asset `arcus-memcached-<v>.tar.gz`; EE keeps tags `<v>-E` with asset `arcus-memcached-EE-<v>.tar.gz`; newest first; `sha256` from the asset `digest` `"sha256:<hex>"`.
  - `pub struct Token(String)` with `Debug`/`Display` = `***`; `pub fn find_token(gh: Option<&Path>) -> Option<Token>`: `GITHUB_TOKEN`, then `<gh> auth token`.
  - `pub fn find_gh(home: &Home) -> Option<PathBuf>`: `gh` on PATH, else `gh_dir()/bin/gh`.
  - `pub fn download_gh(home: &Home) -> io::Result<PathBuf>`: latest `cli/cli` release asset for the OS/arch (`gh_<v>_linux_amd64.tar.gz`, `_linux_arm64.tar.gz`, `_macOS_amd64.zip`, `_macOS_arm64.zip`), verified against the release `gh_<v>_checksums.txt`, extracted into `gh_dir()`.
  - `pub fn parse_device_code(text: &str) -> Option<String>`: the `XXXX-XXXX` code from gh's output (`! First copy your one-time code: 1A2B-3C4D`).
  - `EeLogin` job handler: runs `<gh> auth login --hostname github.com --web --git-protocol https` with stdin `"\n"`, `BROWSER=true`-like no-op (`GH_BROWSER=true`), captures stderr to the job log, writes the code into the job result as soon as it is seen (`{code, url: "https://github.com/login/device"}`), waits for gh to exit, then verifies access with a releases listing. No code within 30 s → fail with gh's output.
  - `pub fn list_releases(edition: Edition, token: Option<&Token>) -> io::Result<Vec<Release>>`; `pub fn download_source(rel: &Release, token: Option<&Token>, home: &Home) -> io::Result<PathBuf>` (`Accept: application/octet-stream`, `Authorization: Bearer` for EE, dest `downloads/<asset_name>`).
  - Tools: `arcus_list_versions {edition}` (EE without a working token → `isError` telling the AI to call `arcus_ee_login`); `arcus_ee_login {}` → submits `EeLogin` (downloading gh first if missing), polls its status up to 30 s for the code, returns `{job_id, code, url}` and text telling the AI to show the code and URL to the user and then poll `arcus_job_status`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn public_filter() { /* tags 1.16.1 (digest sha256:ab) and 1.14.1 → one release 1.16.1, sha256 Some("ab") */ }
#[test] fn ee_tag_asset() { /* tag 0.12.1-E, asset arcus-memcached-EE-0.12.1.tar.gz → version "0.12.1" */ }
#[test] fn device_code_parsed() { assert_eq!(parse_device_code("! First copy your one-time code: 1A2B-3C4D\nOpen this URL..."), Some("1A2B-3C4D".into())); assert_eq!(parse_device_code("nothing"), None); }
#[test] fn token_masked() { assert_eq!(format!("{:?} {}", Token("s".into()), Token("s".into())), "*** ***"); }
#[test] fn gh_asset_names() { /* asset name chosen for (linux, aarch64) == "gh_2.60.0_linux_arm64.tar.gz" given version "2.60.0" */ }
```

- [ ] **Step 2: Run** `cargo test release` — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test release` — Expected: PASS. Manual (macOS): via MCP `arcus_ee_login` returns a code; approve in the browser; `arcus_job_status` → succeeded; `arcus_list_versions {edition:"ee"}` lists `0.12.1`.
- [ ] **Step 5: Commit** `INTERNAL: Add release listing and gh web login for EE`

---

### Task 7: Build pipeline and `arcus_install`

**Files:**
- Modify: `src/server/build.rs`, `src/server/jobs.rs` (register `Install`), `src/mcp/tools.rs`, `Cargo.toml` (add `tar`, `flate2`)

**Interfaces:**
- Consumes: `configure_args`, `build_key`, `validate`, `missing_tools`, `install_hint`, `release::{list_releases, download_source, find_token, find_gh}`, `jobs::{submit, JobCtx, tail}`
- Produces:
  - `pub trait Runner { fn run(&mut self, program: &str, args: &[String], cwd: &Path, log: &Path) -> io::Result<bool>; }` + real impl appending stdout/stderr to `log`.
  - `pub fn build(tarball: &Path, prefix: &Path, features: &BTreeSet<Feature>, log: &Path, runner: &mut dyn Runner, step: &dyn Fn(&str)) -> io::Result<()>`: skip if `<prefix>/bin/memcached` exists; extract into `<prefix>.src/`; run `./deps/install.sh <prefix>`, append `deps/install.log` to `log`, `./configure <configure_args>`, `make`, `make install`; failure → error naming the step, the log path and `tail(log, 20)`.
  - Tool `arcus_install {edition, version?, features: [..]}`: validates features and prerequisites synchronously (missing tools → `isError` with `install_hint`); resolves version (default newest); returns `{job_id, build_key}`. The job downloads then builds into `build_prefix(build_key)`, logging to `jobs/<id>/job.log`; result `{build_key, prefix}`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn build_runs_steps_in_order() { /* tiny tar.gz with one top dir; recording Runner → ["./deps/install.sh","./configure","make","make"], configure args == configure_args(..) */ }
#[test] fn build_failure_names_step() { /* Runner false on "make" → error contains "make" and the log path */ }
#[test] fn install_rejects_invalid_features() { /* arcus_install {edition:"public", features:["repl"]} → isError, no job submitted */ }
#[test] #[ignore] fn build_public_1_16_1() { /* real download + build with {Zk} into a temp home → bin/memcached, lib/default_engine.so, lib/userlog_logger.so exist */ }
```

- [ ] **Step 2: Run** `cargo test build install` — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test` — Expected: PASS. Then `cargo test build_public_1_16_1 -- --ignored` — Expected: PASS.
- [ ] **Step 5: Commit** `INTERNAL: Add source build and arcus_install tool`

---

### Task 8: Local ZooKeeper tools

**Files:**
- Create: `src/server/zookeeper.rs`
- Modify: `src/server/jobs.rs` (register `ZkDownload`), `src/mcp/tools.rs`

**Interfaces:**
- Consumes: `fetch::{fetch_cached, Checksum::Sha512}`, `jobs`, `Home::{zookeeper_dist, zk_dir}`
- Produces:
  - `pub const DEFAULT_ZK_VERSION: &str` (builder picks a current 3.8.x)
  - `pub fn dist_urls(version: &str) -> (String, String)`: `https://archive.apache.org/dist/zookeeper/zookeeper-<v>/apache-zookeeper-<v>-bin.tar.gz` and `+ ".sha512"`; `pub fn parse_sha512_file(text: &str) -> Option<String>`
  - `pub fn zoo_cfg(port: u16, data_dir: &Path) -> String`: `tickTime=2000`, `initLimit=10`, `syncLimit=5`, `dataDir=<abs>`, `clientPort=<port>`, `admin.enableServer=false`, `4lw.commands.whitelist=ruok`
  - `pub fn java_major(version_output: &str) -> Option<u32>` (minimum 8); `pub fn find_java(home: &Home) -> Option<PathBuf>` (`JAVA_HOME`, PATH, then a JRE under `zookeeper/jre/`)
  - `ZkDownload` job: downloads/extracts the ZK dist, and the Temurin 17 JRE for the OS/arch when `download_jre` is true.
  - `pub fn start(home: &Home, port: u16, version: &str) -> io::Result<u32>`: requires dist + java; writes `zk/zoo.cfg`; spawns `java -cp "<dist>/lib/*" org.apache.zookeeper.server.quorum.QuorumPeerMain <zoo.cfg>` detached, output → `zk/zookeeper.log`; writes `zk/zookeeper.pid`; waits up to 10 s for `ruok` → `imok`.
  - `pub fn stop(home: &Home) -> io::Result<()>`
  - Tools: `arcus_zk_start {port?: 2181, version?, download_jre?: false}` — no dist or no java → submits `ZkDownload` and returns `{job_id}` with text to start again after it succeeds (no Java and `download_jre` false → `isError` with install hint and the `download_jre` option); otherwise starts and returns `{ensemble: "127.0.0.1:<port>", pid}`. `arcus_zk_stop {}`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn cfg() { let c = zoo_cfg(2181, Path::new("/d")); assert!(c.contains("clientPort=2181\n") && c.contains("dataDir=/d\n") && c.contains("4lw.commands.whitelist=ruok\n")); }
#[test] fn java_versions() { assert_eq!(java_major("openjdk version \"1.8.0_392\""), Some(8)); assert_eq!(java_major("openjdk version \"17.0.9\" 2023-10-17"), Some(17)); }
#[test] fn sha512_parse() { let h = "a".repeat(128); assert_eq!(parse_sha512_file(&format!("{h}  apache-zookeeper-3.8.4-bin.tar.gz")), Some(h)); }
```

- [ ] **Step 2: Run** `cargo test zookeeper` — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test zookeeper` — Expected: PASS. Manual: `arcus_zk_start` → then `zk_ls {ensemble:"127.0.0.1:2181", path:"/"}` lists `zookeeper`; `arcus_zk_stop` stops it.
- [ ] **Step 5: Commit** `INTERNAL: Add local ZooKeeper tools`

---

### Task 9: Instance config, znodes, `arcus_setup`

**Files:**
- Create: `src/server/instance.rs`, `src/server/znode.rs`
- Modify: `src/zk/mod.rs` (`pub(crate) fn scram_sha256`), `src/mcp/tools.rs`, `Cargo.toml` (add `toml`)

**Interfaces:**
- Consumes: `Edition`, `Feature`, `Home`, `secret::{random_password, write_secret}`, `scram_sha256`
- Produces:
  - `#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)] pub struct InstanceConfig { pub name: String, pub edition: Edition, pub version: String, pub features: BTreeSet<Feature>, pub build_key: String, pub port: u16, pub memory_mb: u32, pub zk: Option<ZkTarget>, pub service_code: Option<String>, pub sasl: Option<SaslConfig>, pub repl: Option<ReplConfig> }`; `ZkTarget { ensemble: String, local: bool }`; `SaslConfig { acl_group: String, user: Option<String> }`; `ReplConfig { group: String, listen: String }`; `load(&Path)`, `save(&Path)` (`instance.toml`).
  - `pub fn server_args(cfg: &InstanceConfig, prefix: &Path) -> Vec<String>`: `-E <prefix>/lib/default_engine.so -X <prefix>/lib/userlog_logger.so -p <port> -m <memory_mb>` + `-z <ensemble>` with zk + `-S` with sasl.
  - `pub fn local_ip_for(ensemble: &str) -> io::Result<IpAddr>`: `ARCUS_CACHE_PUBLIC_IP` if set, else UDP `connect` to the first ensemble host and `local_addr()`.
  - `#[derive(Debug, PartialEq, Serialize)] pub struct ZnodeSpec { pub path: String, pub value: Vec<u8> }`
  - `pub fn plan(cfg: &InstanceConfig, ip: IpAddr, password_secret: Option<&str>) -> Vec<ZnodeSpec>` (parents first): root `r` = `/arcus_repl` if repl else `/arcus`: `r`, `r/cache_list`, `r/cache_list/<svc>`, `r/cache_server_mapping`, `r/cache_server_mapping/<ip>:<port>`, child `<svc>` or `<svc>^<group>^<listen>`, `r/cache_server_log`; repl adds `r/group_list`, `r/group_list/<svc>`, `r/group_list/<svc>/<group>`; sasl adds `/arcus_acl_mapping`, `/arcus_acl_mapping/<svc>` = `<acl_group>`, and when `zk.local` with a user: `/arcus_acl`, `/arcus_acl/<group>`, `/arcus_acl/<group>/<user>` (permission value confirmed from EE `sasl_auxprop.c`), `/arcus_acl/<group>/<user>/authPassword` = secret; migration adds the `cloud_stat` nodes confirmed from EE `arcus_zk.c`. Record the confirmed sources in code comments.
  - `pub trait ZnodeStore { fn get(&self, p: &str) -> io::Result<Option<Vec<u8>>>; fn create(&self, p: &str, v: &[u8]) -> io::Result<()>; fn set(&self, p: &str, v: &[u8]) -> io::Result<()>; fn children(&self, p: &str) -> io::Result<Vec<String>>; }` over `zookeeper::ZooKeeper` (persistent, open ACL) and an in-memory test store.
  - `pub enum Action { Create, Same, Differs { current: Vec<u8> } }`; `pub fn diff(store, plan) -> io::Result<Vec<(ZnodeSpec, Action)>>` (empty planned value on a parent → `Same`).
  - `pub fn apply(store, diffs, overwrite: &[String]) -> io::Result<Vec<String>>` (creates `Create`s; sets `Differs` only when its path is in `overwrite`; returns what was written).
  - `pub fn verify(store, cfg, ip) -> Vec<String>`: `mapping <path> has N children (server needs exactly 1)`, `/arcus_acl_mapping/<svc> is empty`, `ACL group /arcus_acl/<g> does not exist`.
  - Tool `arcus_setup {name, edition, version, features, port?: 11211, memory_mb?: 64, zk: "local" | "<ensemble>", service_code (required when zk), acl_group (required when sasl), acl_user? (local zk only), repl_group (required when repl), repl_listen?, apply?: false, overwrite?: []}`: requires an existing build for `build_key`; existing ensemble + sasl + missing `/arcus_acl/<group>` → `isError` before writing; local zk + sasl user → `random_password` written to `instances/<name>/sasl_password`, SCRAM secret in the plan; without `apply` returns the diff list; with `apply` writes, verifies, saves `instance.toml`, returns written paths, problems and the password file path.

- [ ] **Step 1: Write the failing tests** (in-memory store)

```rust
#[test] fn plan_plain() { /* contains /arcus/cache_list/svc1 and /arcus/cache_server_mapping/10.0.0.1:11211/svc1 */ }
#[test] fn plan_repl() { /* contains /arcus_repl/cache_server_mapping/10.0.0.1:11211/svc1^g1^10.0.0.1:20211 and /arcus_repl/group_list/svc1/g1 */ }
#[test] fn plan_sasl_existing_only_mapping() { /* contains ZnodeSpec{"/arcus_acl_mapping/svc1", b"grp"}; nothing under /arcus_acl/ */ }
#[test] fn differs_kept_unless_listed() { /* store /arcus_acl_mapping/svc1="old": apply with overwrite [] keeps "old"; with ["/arcus_acl_mapping/svc1"] sets "grp" */ }
#[test] fn no_apply_writes_nothing() { /* tool without apply on empty store → store empty, result lists Create entries */ }
#[test] fn verify_problems() { /* empty mapping value and two mapping children → 2 problems */ }
#[test] fn missing_names_ask_user() { /* tool with zk but no service_code → isError text contains "service_code" and "ask the user" */ }
#[test] fn server_args_full() { assert_eq!(server_args(&cfg_sasl_local(), Path::new("/b")),
    ["-E","/b/lib/default_engine.so","-X","/b/lib/userlog_logger.so","-p","11211","-m","64","-z","127.0.0.1:2181","-S"]); }
#[test] fn toml_roundtrip() {}
```

- [ ] **Step 2: Run** `cargo test instance znode setup` — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test` — Expected: PASS.
- [ ] **Step 5: Commit** `INTERNAL: Add instance config, znode provisioning and arcus_setup`

---

### Task 10: Supervisor and start/stop/status tools

**Files:**
- Create: `src/server/supervise.rs`
- Modify: `src/main.rs` (`--supervise <name>`), `src/mcp/tools.rs`, `Cargo.toml` (add `libc`)

**Interfaces:**
- Consumes: `InstanceConfig`, `server_args`, `Home`, `jobs::tail`
- Produces:
  - `#[derive(Serialize, Deserialize, Debug, PartialEq)] pub struct ExitRecord { pub time: String /* RFC 3339 UTC */, pub code: Option<i32>, pub signal: Option<i32> }`; `pub fn exit_record(status: ExitStatus, time: String) -> ExitRecord`
  - `pub fn supervise(home: &Home, name: &InstanceName) -> io::Result<()>`: writes `supervisor.pid`; spawns `<prefix>/bin/memcached <server_args>` with `current_dir(instance dir)`, stdin null, stdout/stderr appended to `run.log`; writes `server.pid`; `wait()`; writes `last_exit.json`; removes pid files. The single place where project 2 hooks alerts.
  - `pub fn is_running(pid_file: &Path, expect_comm: &str) -> bool`: `kill(pid, 0)` succeeds **and** the command name contains `expect_comm` (Linux `/proc/<pid>/comm`; macOS `ps -p <pid> -o comm=`).
  - `pub fn start(home, name) -> io::Result<u32>`: refuses if running; spawns `current_exe() --supervise <name> --home <root>` with `process_group(0)`, stdio null/log; waits up to 5 s for `version\r\n` → `VERSION ` on the port; on timeout returns an error with the last 20 lines of the newest `ARCUSlog/*` file and `run.log`.
  - `pub fn stop(home, name, force: bool) -> io::Result<StopOutcome>` with `pub enum StopOutcome { Stopped, StillRunning, Killed, NotRunning }`: SIGTERM, wait up to 10 s; SIGKILL only with `force`.
  - `#[derive(Serialize)] pub struct Status { pub name: String, pub running: bool, pub pid: Option<u32>, pub port: u16, pub responding: bool, pub last_exit: Option<ExitRecord> }`; `pub fn status(home, name: Option<&InstanceName>) -> io::Result<Vec<Status>>`
  - Tools: `arcus_start {name}`, `arcus_stop {name, force?: false}` (StillRunning text tells the AI to ask the user before calling again with `force: true`), `arcus_status {name?}`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn exit_by_signal() { use std::os::unix::process::ExitStatusExt;
    assert_eq!(exit_record(ExitStatus::from_raw(9), "t".into()), ExitRecord { time: "t".into(), code: None, signal: Some(9) }); }
#[test] fn exit_by_code() { use std::os::unix::process::ExitStatusExt;
    assert_eq!(exit_record(ExitStatus::from_raw(3 << 8), "t".into()).code, Some(3)); }
#[test] fn foreign_or_dead_pid_not_running() { /* pid file with this test's own pid and expect "memcached" → false; pid 999999 → false */ }
```

- [ ] **Step 2: Run** `cargo test supervise` — Expected: FAIL.
- [ ] **Step 3: Implement** (RFC 3339 formatting from `SystemTime` with a small civil-from-days helper; no new crate).
- [ ] **Step 4: Run** `cargo test supervise` — Expected: PASS. Manual: with the Task 7 build and a Task 9 instance without zk: `arcus_start` → `arcus_status` running+responding; `kill -9` the server → `arcus_status` shows `last_exit.signal == 9`; `instances/<name>/ARCUSlog/` exists.
- [ ] **Step 5: Commit** `INTERNAL: Add server supervisor and start/stop/status tools`

---

### Task 11: Client setup docs and end-to-end check

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Document** a "MCP mode" section: what `--mcp` does; registration examples (`claude mcp add arcus -- arcus-cli --mcp`; Gemini CLI `~/.gemini/settings.json` `mcpServers.arcus.command`; Codex `~/.codex/config.toml` `[mcp_servers.arcus]`); the tool list; EE login flow; where files live under `~/.arcus-cli/`.
- [ ] **Step 2: End-to-end on macOS and one Linux host through Claude Code:**
  1. "Install public ARCUS with ZK and SASL, local ZooKeeper, service code test, ACL group g1, user u1" → AI calls `arcus_list_features`, `arcus_check_prerequisites`, `arcus_install` (+ `arcus_job_status`), `arcus_zk_start`, `arcus_setup` (dry run, then `apply`), `arcus_start`.
  2. `arcus-cli --sasl --port 11211` authenticates as `u1` with the password from `instances/<name>/sasl_password`; `set`/`get` work.
  3. EE: `arcus_ee_login` → approve in browser → `arcus_install {edition:"ee", features:["zk","repl"]}` → two instances `r1`, `r2` in group `g1` on 11211/11212 → both appear under `/arcus_repl/group_list/test/g1` (`zk_ls`).
  4. `cargo test` and `cargo clippy` — no new warnings in `src/mcp`, `src/server`.
- [ ] **Step 3: Commit** `DOC: Document MCP mode`
