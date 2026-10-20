# Remote Deploy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** From the Mac's `arcus-cli --mcp`, register machines (reusing `~/.ssh/config`), install a resident `arcus-cli --agent` on them over SSH, run every existing tool on a chosen machine, and build a ZooKeeper ensemble and ARCUS servers across machines.

**Architecture:** A `machine` argument on existing tools routes the call over SSH to `arcus-cli --call <tool>` on that machine, which runs the same tool code and prints the `ToolResult` as JSON. All SSH goes through one runner (`src/remote/ssh.rs`) using the system `ssh`/`scp` with ControlMaster and an `SSH_ASKPASS` dialog. On each machine `arcus-cli --agent` is a Linux child subreaper that manages one `--supervise` process per server or ZooKeeper node, so agent restarts never stop servers and exit codes are kept.

**Tech Stack:** Rust 2024, synchronous I/O, existing crates (`serde_json`, `toml`, `ureq`, `tar`/`flate2`, `sha2`, `libc`); system `ssh`, `scp`, `ssh-keyscan`, `ssh-keygen`, `osascript` (macOS).

**Spec:** `docs/superpowers/specs/2026-10-20-remote-deploy-design.md` (builds on `docs/superpowers/specs/2026-10-19-server-install-design.md`)

## Global Constraints

- Everything from the server-install plan still holds: synchronous code, stdout carries only JSON-RPC in `--mcp` mode (and only the one result line in `--call` mode), secrets never appear in tool output or logs, `structuredContent` is always an object, existing REPL usage unchanged.
- The machine-selection argument is named `machine` (not `host`; `memcached_command` already has `host`).
- Machines in `~/.ssh/config` (non-wildcard `Host` aliases, including `Include`d files) are used by alias with no questions; others are registered with `host`, `user`, `port` (recommended 22), `identity_file?` in `~/.arcus-cli/hosts.toml`. Passwords are never stored.
- SSH options added on top of the user's ssh config: `ControlMaster=auto`, `ControlPath=~/.arcus-cli/ssh/%C`, `ControlPersist=10m`. Passwords come only from the `SSH_ASKPASS` dialog (`SSH_ASKPASS_REQUIRE=force`); without a GUI, an auth failure returns a hint to run `ssh-copy-id <user>@<host>` once.
- Unknown host keys: return the fingerprint; proceed only when called again with `accept_host_key: true` (then `StrictHostKeyChecking=accept-new`).
- plan/apply (two calls) only for `arcus_host_install` (install/replace) and host-key acceptance. Other remote calls keep their existing gates.
- Remote EE login needs `accept_plaintext_token: true` when the machine has no keyring (token saved by gh in `~/.config/gh/hosts.yml`, mode 600).
- Agent: `arcus-cli --agent --home <dir>`, socket `<home>/agent.sock` (mode 600), one `--supervise` per server/ZK node, `PR_SET_CHILD_SUBREAPER` on Linux, service unit uses `KillMode=process`; nothing ever stops a server or ZK node because of an agent or supervisor failure.
- Service mode on Linux: systemd user unit + linger; if `loginctl enable-linger` is refused, stop and offer "ask an admin for linger" or "install in daemon mode". Never sudo.
- Release assets: `arcus-cli-<tag>-<target>.tar.gz` (contains `arcus-cli-<tag>-<target>/arcus-cli`) + `.sha256`; targets `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`. Binary tag from compile-time `ARCUS_CLI_TAG` (`dev` if unset).

## Review Focus

- A machine alias from `~/.ssh/config` that uses `ProxyJump`/`Include`/`Match`: arcus-cli must not rewrite or bypass it — always pass the alias to `ssh` (Task 3).
- `--call` printing anything besides the single result line (warnings, panics, child output): the Mac side must still parse the result or return a clear error, never hang (Task 2, Task 5).
- A second `arcus_host_install` while an agent is already running: report version and managed servers; never replace without `replace: true` (Task 8).
- Agent restart with servers running: servers keep running and are re-listed by the new agent (Task 7).
- ZK peers that cannot reach each other (security group): the cluster start must report which node pairs fail instead of waiting forever (Task 9).

---

## File Structure

| File | Responsibility |
|---|---|
| `build.rs` (new, crate root) / `src/main.rs` | Embed `ARCUS_CLI_TAG`; new flags `--call`, `--agent`, `--askpass`, `--supervise-zk` |
| `.github/workflows/release.yml` | Pass `ARCUS_CLI_TAG=${GITHUB_REF_NAME}` to the build |
| `src/mcp/mod.rs` | `ToolResult` (de)serialization; `run_call` for `--call` |
| `src/remote/mod.rs` | module root |
| `src/remote/machines.rs` | ssh config aliases, `hosts.toml`, machine resolution |
| `src/remote/ssh.rs` | ssh/scp runner, options, askpass, host keys, error classes |
| `src/remote/forward.rs` | `machine` routing wrapper around tools; version check |
| `src/remote/dist.rs` | release binary download/cache, target selection |
| `src/remote/install.rs` | host check/install plan & apply, unit/plist/daemon text |
| `src/mcp/remote_tools.rs` | new MCP tools: `arcus_host_*`, `arcus_cluster_*` |
| `src/server/agent.rs` | agent: socket, subreaper, reaping, orphan watch |
| `src/server/supervise.rs`, `src/server/zookeeper.rs` | ZK nodes under supervisors; node addresses |

---

### Task 1: Release tag in the binary and `--call` mode

**Files:** Create `build.rs`; modify `src/main.rs`, `src/mcp/mod.rs`, `.github/workflows/release.yml`.

**Interfaces:**
- Produces:
  - `pub const TAG: &str` in `src/main.rs` = `option_env!("ARCUS_CLI_TAG").unwrap_or("dev")` (re-run trigger in `build.rs`: `cargo:rerun-if-env-changed=ARCUS_CLI_TAG`); `--version` prints `arcus-cli <CARGO_PKG_VERSION> (<TAG>)`.
  - `impl ToolResult { pub fn to_json(&self) -> Value /* {"text","structured","is_error"} */; pub fn from_json(v: &Value) -> Option<ToolResult> }`
  - `pub fn run_call(tool: &str, input: impl Read, output: impl Write, tools: Vec<Box<dyn Tool>>, ctx: Ctx) -> io::Result<bool>`: reads one JSON object (arguments) from `input`, runs the tool (with the same panic catching as `serve`), writes exactly one line `{"arcus_call": 1, "tag": TAG, "result": <to_json>}`; unknown tool → result with `is_error`. Returns `!is_error`.
  - `main.rs`: `--call <tool>` (hidden, group "mode", accepts `--home`) → `run_call(...)`; exit 0/1.
  - release.yml Build step gets `env: ARCUS_CLI_TAG: ${{ github.ref_name }}`.

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn tool_result_roundtrip() {
    let r = ToolResult { text: "t".into(), structured: Some(json!({"a":1})), is_error: true };
    assert_eq!(ToolResult::from_json(&r.to_json()).unwrap(), r); // derive PartialEq
}
#[test] fn call_runs_tool_and_prints_one_line() {
    // Echo test tool; input `{"text":"hi"}` → exactly one output line, parsed: arcus_call==1, result.text=="hi", tag non-empty
}
#[test] fn call_unknown_tool_is_error() { /* result.is_error == true, still one line */ }
```
main.rs test: `Args::try_parse_from(["arcus-cli","--call","zk_ls","--home","/x"])` parses.

- [ ] **Step 2: Run** `cargo test call tool_result main` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test` — PASS; `printf '{"edition":"public"}' | cargo run -q -- --call arcus_list_features` prints one JSON line.
- [ ] **Step 5: Commit** `INTERNAL: Embed the release tag and add --call mode`

---

### Task 2: Machines and `~/.ssh/config`

**Files:** Create `src/remote/mod.rs`, `src/remote/machines.rs`; modify `src/main.rs` (`mod remote;`), `src/server/home.rs` (`hosts_file()` → `<root>/hosts.toml`, `ssh_dir()` → `<root>/ssh`, `dist_dir()` → `<root>/dist`).

**Interfaces:**
- Produces:
  - `pub fn ssh_config_aliases(path: &Path) -> io::Result<Vec<String>>`: non-wildcard `Host` names (skip patterns containing `*`, `?`, `!`), following `Include` (relative to `~/.ssh`, glob `*` supported), case-insensitive keywords, order kept, deduped.
  - `#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)] pub enum Target { SshAlias, Explicit { host: String, user: String, port: u16, identity_file: Option<String> } }`
  - `#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)] pub struct Machine { pub name: String, pub target: Target }` with `fn ssh_destination(&self) -> Vec<String>` (alias → `[name]`; explicit → `["-p", port, "-l", user, (+ "-i", file), host]`).
  - `pub fn load_machines(home: &Home) -> io::Result<Vec<Machine>>`, `pub fn save_machine(home: &Home, m: &Machine) -> io::Result<()>` (`hosts.toml`, upsert by name).
  - `pub fn resolve(home: &Home, name: &str, ssh_config: &Path) -> Result<Machine, String>`: registered machine, else ssh alias (returns `SshAlias`), else error telling the AI to call `arcus_host_add` with connection details.
  - Machine names reuse `InstanceName` rules.

- [ ] **Step 1: Write failing tests** (fixture configs in a temp dir)

```rust
#[test] fn aliases_skip_wildcards_and_follow_include() {
    // main: "Host dev-1\n  HostName 10.0.0.1\nHost *\n  User x\nInclude conf.d/*\n"; conf.d/a: "host dev-2 dev-3\n"
    assert_eq!(ssh_config_aliases(&main).unwrap(), ["dev-1", "dev-2", "dev-3"]);
}
#[test] fn resolve_prefers_registered_then_alias() { /* hosts.toml has dev-9 explicit; config has dev-1 → dev-1 SshAlias, dev-9 Explicit, nope → Err containing "arcus_host_add" */ }
#[test] fn explicit_destination_args() { /* host h, user u, port 2222, identity Some("/k") → ["-p","2222","-l","u","-i","/k","h"] */ }
```

- [ ] **Step 2: Run** `cargo test machines` — FAIL. **Step 3: Implement.** **Step 4: Run** `cargo test` — PASS.
- [ ] **Step 5: Commit** `INTERNAL: Resolve remote machines from ssh config and hosts.toml`

---

### Task 3: SSH runner, askpass, host keys

**Files:** Create `src/remote/ssh.rs`; modify `src/main.rs` (`--askpass` hidden flag: positional prompt).

**Interfaces:**
- Consumes: `Machine`, `Home::ssh_dir`.
- Produces:
  - `pub fn common_opts(home: &Home) -> Vec<String>`: `-o ControlMaster=auto -o ControlPath=<ssh_dir>/%C -o ControlPersist=10m -o ConnectTimeout=15` (creates `ssh_dir` mode 700).
  - `pub enum HostKeyPolicy { Strict, AcceptNew }` → `StrictHostKeyChecking=yes|accept-new`.
  - `pub struct SshOutput { pub status: i32, pub stdout: Vec<u8>, pub stderr: String }`
  - `pub trait Runner { fn run(&self, program: &str, args: &[String], stdin: &[u8], env: &[(String, String)]) -> io::Result<SshOutput>; }` + `SystemRunner`.
  - `pub fn askpass_env(gui: bool) -> Vec<(String, String)>`: when `gui`, `SSH_ASKPASS=<current_exe>`, `SSH_ASKPASS_REQUIRE=force`, `ARCUS_ASKPASS=1`, and `DISPLAY=:0` if unset on macOS; else `SSH_ASKPASS_REQUIRE=never` plus `-o BatchMode=yes` handled by the caller.
  - `pub fn gui_available() -> bool`: macOS → true unless `SSH_CONNECTION` is set; Linux → `DISPLAY` or `WAYLAND_DISPLAY` set.
  - `--askpass <prompt>` (main.rs): macOS runs `osascript` `display dialog "<prompt>" default answer "" with hidden answer` and prints the answer to stdout (this is ssh's askpass protocol, a separate process — not the MCP stdout); non-macOS GUI: `zenity --password` if present, else exit 1. Prompts containing "yes/no" (host key) are answered "no" — host keys are handled via `accept_host_key`.
  - `pub enum SshError { HostKeyUnknown { fingerprint: String }, HostKeyChanged, AuthFailed { hint: String }, Unreachable(String), Remote { status: i32, stderr: String } }` and `pub fn classify(stderr: &str, status: i32, dest_label: &str) -> Option<SshError>` (`Host key verification failed` / `No ... host key is known` → unknown or changed by `REMOTE HOST IDENTIFICATION HAS CHANGED`; `Permission denied (` → AuthFailed with hint `run "ssh-copy-id <dest>" once in a terminal`; `Could not resolve hostname`/`Connection refused`/`timed out` → Unreachable).
  - `pub fn fingerprint(runner: &dyn Runner, host: &str, port: u16) -> io::Result<String>`: `ssh-keyscan -p <port> <host>` piped to `ssh-keygen -lf -`.
  - `pub struct Ssh<'a> { home, runner: &'a dyn Runner, machine: &'a Machine, policy: HostKeyPolicy }` with `fn exec(&self, remote_cmd: &str, stdin: &[u8]) -> Result<SshOutput, SshError>` and `fn copy(&self, local: &Path, remote: &str) -> Result<(), SshError>` (scp with the same opts; destination `<alias or user@host>:<remote>`, explicit port via `-P`). For `HostKeyUnknown`, `exec` fills the fingerprint using `ssh -G <dest>` to learn hostname/port.

- [ ] **Step 1: Write failing tests** (fake `Runner` recording argv/env)

```rust
#[test] fn exec_argv_for_alias() { /* argv starts with common_opts, then -o StrictHostKeyChecking=yes, then "dev-1", then the command */ }
#[test] fn classify_errors() {
    assert!(matches!(classify("Host key verification failed.", 255, "dev-1"), Some(SshError::HostKeyUnknown{..})));
    assert!(matches!(classify("@@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @@@", 255, "d"), Some(SshError::HostKeyChanged)));
    match classify("u@h: Permission denied (publickey,password).", 255, "u@h") { Some(SshError::AuthFailed{hint}) => assert!(hint.contains("ssh-copy-id u@h")), o => panic!("{o:?}") }
}
#[test] fn askpass_env_gui_and_headless() { /* gui=true → SSH_ASKPASS_REQUIRE=force; gui=false → never */ }
#[test] fn scp_uses_capital_p_for_explicit_port() {}
```

- [ ] **Step 2: Run** `cargo test ssh` — FAIL. **Step 3: Implement.** **Step 4: Run** `cargo test` — PASS.
- [ ] **Step 5: Manual (macOS):** `cargo run -- --askpass "test"` shows a hidden-input dialog and prints the typed text.
- [ ] **Step 6: Commit** `INTERNAL: Add SSH runner with askpass dialog and host key handling`

---

### Task 4: Machine tools

**Files:** Create `src/mcp/remote_tools.rs`; modify `src/mcp/tools.rs` (`registry()` appends `remote_tools::registry()`), `src/mcp/mod.rs`.

**Interfaces:**
- Consumes: Tasks 2–3.
- Produces tools:
  - `arcus_host_list_ssh_config {}` → `{aliases: [...]}` from `~/.ssh/config`.
  - `arcus_host_add {name, host?, user?, port? (recommended 22), identity_file?}`: if `name` is an ssh alias and no `host` given → save `SshAlias`, return `ssh -G <name>` resolved `hostname`, `user`, `port`; else `host` and `user` required (missing → "ask the user"), save `Explicit`.
  - `arcus_host_list {}` → registered machines.
  - `arcus_host_check {machine, accept_host_key?: false}`: runs `uname -sm; echo $HOME; ~/.arcus-cli/bin/arcus-cli --version 2>/dev/null; test -S ~/.arcus-cli/agent.sock && ~/.arcus-cli/bin/arcus-cli --agent-status --home ~/.arcus-cli` in one `exec`; returns `{machine, os, arch, target, home, arcus_cli: {version, tag}|null, agent: {running, servers:[...]}|null}`. `HostKeyUnknown` → isError with the fingerprint and text telling the AI to show it and call again with `accept_host_key: true` after the user agrees. `AuthFailed` → isError with the hint.
  - `pub fn target_for(uname_s: &str, uname_m: &str) -> Option<&'static str>` (Linux/x86_64, Linux/aarch64|arm64, Darwin/x86_64, Darwin/arm64).
  - (`--agent-status` is implemented in Task 7; until then `arcus_host_check` treats a non-zero exit as `agent: null`.)

- [ ] **Step 1: Write failing tests** (fake runner)

```rust
#[test] fn target_mapping() { assert_eq!(target_for("Linux","aarch64"), Some("aarch64-unknown-linux-gnu")); assert_eq!(target_for("Darwin","arm64"), Some("aarch64-apple-darwin")); assert_eq!(target_for("SunOS","x"), None); }
#[test] fn host_add_alias_needs_no_details() {}
#[test] fn host_add_explicit_requires_user() { /* missing user → isError "ask the user" */ }
#[test] fn host_check_parses_output() { /* fake stdout "Linux aarch64\n/home/u\narcus-cli 1.0.0 (v1.1.0)\n" → os Linux, target aarch64-unknown-linux-gnu, tag v1.1.0, agent null */ }
#[test] fn host_check_unknown_key_returns_fingerprint() {}
```

- [ ] **Step 2–4:** RED, implement, GREEN (`cargo test remote_tools`, then `cargo test`).
- [ ] **Step 5: Commit** `INTERNAL: Add machine registration and check tools`

---

### Task 5: `machine` routing for existing tools

**Files:** Create `src/remote/forward.rs`; modify `src/mcp/tools.rs` (wrap forwardable tools in `registry()`).

**Interfaces:**
- Consumes: Tasks 1–4.
- Produces:
  - `pub struct Remote<T: Tool>(pub T)`: `spec()` = inner spec with a `machine` property added (`"Machine name (see arcus_host_list); omit to run on this computer"`); `call()`: no `machine` → inner; with `machine` → `forward(ctx, name, args_without_machine, machine)`.
  - Forwardable: every existing tool except `arcus_host_*` and `arcus_cluster_*`.
  - `pub fn forward(ctx: &Ctx, tool: &str, args: &Value, machine: &str, runner: &dyn Runner) -> ToolResult`: resolve machine; `exec("~/.arcus-cli/bin/arcus-cli --call <tool> --home ~/.arcus-cli", args JSON)`; find the line starting with `{"arcus_call":` in stdout; parse `result`; add `"machine": name` into structured (create `{}` if None); if the remote `tag` differs from local `TAG` and neither is `dev`, append a one-line note to `text`. Missing remote binary (exit 127 / "No such file") → isError telling the AI to run `arcus_host_install`. No parseable line → isError with the first 20 lines of stdout/stderr.
  - `arcus_job_status` forwarded likewise (job ids are per machine).

- [ ] **Step 1: Write failing tests** (fake runner returning canned stdout)

```rust
#[test] fn schema_gets_machine_property() {}
#[test] fn forward_parses_result_and_tags_machine() { /* stdout "warning\n{\"arcus_call\":1,\"tag\":\"v1\",\"result\":{\"text\":\"ok\",\"structured\":{\"a\":1},\"is_error\":false}}\n" → text "ok", structured a==1 && machine=="dev-1" */ }
#[test] fn forward_missing_binary_hints_install() { /* status 127 → isError containing "arcus_host_install" */ }
#[test] fn forward_unparseable_output_is_error() {}
#[test] fn no_machine_runs_locally() {}
```

- [ ] **Step 2–4:** RED, implement, GREEN.
- [ ] **Step 5: Commit** `INTERNAL: Route tool calls to remote machines over SSH`

---

### Task 6: Release binaries for remote machines

**Files:** Create `src/remote/dist.rs`.

**Interfaces:**
- Consumes: `fetch::{fetch_cached, Checksum::Sha256, Request}`, `build::extract` (tar.gz), `Home::dist_dir`.
- Produces:
  - `pub fn asset_names(tag: &str, target: &str) -> (String, String)` → (`arcus-cli-<tag>-<target>.tar.gz`, `<...>.tar.gz.sha256`).
  - `pub fn parse_sha256_file(text: &str) -> Option<String>` (first 64-hex token).
  - `pub fn fetch_binary(home: &Home, tag: &str, target: &str) -> io::Result<PathBuf>`: `https://github.com/ing-eoking/arcus-cli/releases/download/<tag>/<asset>`, checksum verified, extracted, cached at `dist/<tag>/<target>/arcus-cli` (mode 755).
  - `pub fn choose_binary(home, target, tag_arg: Option<&str>, binary_arg: Option<&Path>) -> Result<(PathBuf, String /*tag*/), String>`: `binary_arg` wins (tag reported as `custom`); else `tag_arg`; else local `TAG` if not `dev`; else error telling the AI that this build has no release tag and to pass `binary` (a locally built Linux binary) or `tag`.

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn names() { assert_eq!(asset_names("v1.2.0","aarch64-unknown-linux-gnu").0, "arcus-cli-v1.2.0-aarch64-unknown-linux-gnu.tar.gz"); }
#[test] fn sha_file() { let h="a".repeat(64); assert_eq!(parse_sha256_file(&format!("{h}  x.tar.gz")), Some(h)); }
#[test] fn dev_build_without_tag_or_binary_is_error() {}
#[test] fn binary_arg_wins() {}
```

- [ ] **Step 2–4:** RED, implement, GREEN.
- [ ] **Step 5: Commit** `INTERNAL: Fetch release binaries for remote machines`

---

### Task 7: Agent with subreaper and supervised ZooKeeper nodes

**Files:** Create `src/server/agent.rs`; modify `src/server/supervise.rs`, `src/server/zookeeper.rs`, `src/main.rs` (`--agent`, `--agent-status`, `--supervise-zk <id>`), `src/mcp/tools.rs` (start/stop/zk tools delegate to the agent when it is running).

**Interfaces:**
- Consumes: `supervise`, `zookeeper`, `InstanceConfig`.
- Produces:
  - ZooKeeper nodes run under `arcus-cli --supervise-zk <id> --home <root>`: the supervisor spawns java for node `<id>` from `zk/node<id>/` (as today), writes `zk/node<id>/server.pid` and `supervisor.pid`, waits, writes `zk/node<id>/last_exit.json`. `zookeeper::start` spawns supervisors instead of java directly; readiness logic unchanged.
  - `pub fn run_agent(home: &Home) -> io::Result<()>`: on Linux `prctl(PR_SET_CHILD_SUBREAPER, 1)`; binds `agent.sock` (remove stale socket if no one answers; mode 600); writes `agent.pid`; thread A accepts connections, one JSON line request → one JSON line reply; thread B reaps: `waitpid(-1, WNOHANG)` loop on SIGCHLD (or 1 s tick) — for each reaped pid that matches an instance's `server.pid` or a ZK node's `server.pid` (an orphan adopted after its supervisor died) write that `last_exit.json` with code/signal; thread C watches servers whose supervisor and parent are both gone (startup scan): Linux `pidfd_open` + poll, macOS `kqueue` `EVFILT_PROC NOTE_EXIT` → `last_exit.json` with `code: null, signal: null, note: "exit status unknown"`.
  - Agent requests: `{"op":"start","name"}`, `{"op":"stop","name","force"}`, `{"op":"status","name"?}`, `{"op":"zk_start", nodes, version, members?}`, `{"op":"zk_stop"}`, `{"op":"info"}` → `{tag, pid, servers:[...], zk_nodes:[...]}`. Supervisors are spawned by the agent (so they are its children); server processes stay children of their supervisors.
  - `pub fn agent_request(home: &Home, req: &Value) -> io::Result<Value>`; tools call it when `agent.sock` answers, else use today's direct path.
  - `--agent-status`: prints `info` JSON (one line) or exits 1 if no agent.
  - `ExitRecord` gains `note: Option<String>` (`#[serde(default)]`).
  - Restart behaviour: a new agent reads pid files, keeps existing supervisors (does not respawn), reports them in `info`.

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn agent_protocol_info_roundtrip() { /* run_agent in a thread on a temp home; agent_request {"op":"info"} → pid == process id, servers == [] ; then shut down via {"op":"shutdown"} (test-only op behind cfg(test)) */ }
#[test] fn orphan_exit_record_has_unknown_note() { /* unit-test the record builder */ }
#[test] #[cfg(target_os = "linux")] fn subreaper_adopts_grandchild() { /* spawn `sh -c 'sleep 5 & exit'` from a thread that set subreaper; waitpid sees the sleep pid after kill */ }
#[test] fn stale_socket_is_replaced() {}
```

- [ ] **Step 2–4:** RED, implement, GREEN.
- [ ] **Step 5: Manual (macOS, temp home, server port 21611, ZK client port 22181):**
  1. Start the agent; `arcus_start` via MCP goes through the agent.
  2. `kill -9` the agent → the server still answers `version`.
  3. Start the agent again → `arcus_status` lists the server.
  4. `kill -9` the server's supervisor → the server keeps running; `kill` the server → `last_exit.json` has note "exit status unknown" (macOS has no subreaper).
  5. Stop everything and remove the temp home.
- [ ] **Step 6: Commit** `INTERNAL: Add resident agent with subreaper and supervised ZooKeeper nodes`

---

### Task 8: `arcus_host_install`

**Files:** Create `src/remote/install.rs`; modify `src/mcp/remote_tools.rs`.

**Interfaces:**
- Consumes: Tasks 3, 4, 6, 7.
- Produces:
  - `pub fn systemd_unit(bin: &str, home: &str) -> String`: `[Unit] Description=arcus-cli agent` / `[Service] ExecStart=<bin> --agent --home <home>` / `Restart=on-failure` / `KillMode=process` / `[Install] WantedBy=default.target`.
  - `pub fn launchd_plist(bin: &str, home: &str) -> String` (label `com.arcus-cli.agent`, `RunAtLoad` true, `KeepAlive` `{SuccessfulExit: false}`, `AbandonProcessGroup` true).
  - `pub fn daemon_command(bin: &str, home: &str) -> String`: `setsid nohup <bin> --agent --home <home> >> <home>/agent.log 2>&1 < /dev/null &` (`setsid` omitted on macOS).
  - Tool `arcus_host_install {machine, mode: "service"|"daemon" (recommended service), tag?, binary?, replace?: false, apply?: false}`:
    1. `arcus_host_check` data; existing agent and not `replace` → isError with its version and managed servers, telling the AI to ask the user.
    2. Linux service: check linger (`loginctl show-user $USER -p Linger`); if `no`, plan includes `loginctl enable-linger`; on apply, if that command fails → isError explaining the cluster would stop after logout and offering `mode: "daemon"` or asking an admin to enable linger (and the exact admin command `sudo loginctl enable-linger <user>`).
    3. Without `apply`: return the plan `{binary_source, tag, remote_path: "~/.arcus-cli/bin/arcus-cli", mode, steps: [...] }`.
    4. With `apply`: `choose_binary` → `scp` to `~/.arcus-cli/bin/arcus-cli.new`, `chmod 755`, atomic `mv`; with `replace`, stop the old agent only (`systemctl --user stop` / `launchctl bootout` / `kill <agent.pid>`) — supervisors and servers keep running; install unit/plist/daemon and start; poll `--agent-status` up to 10 s; return `arcus_host_check` data.

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn unit_has_killmode_process() { assert!(systemd_unit("/b","/h").contains("KillMode=process\n")); }
#[test] fn plist_abandons_process_group() {}
#[test] fn install_refuses_running_agent_without_replace() {}
#[test] fn install_plan_without_apply_writes_nothing() { /* fake runner sees only the check command */ }
#[test] fn linger_refused_offers_daemon() {}
```

- [ ] **Step 2–4:** RED, implement, GREEN.
- [ ] **Step 5: Commit** `INTERNAL: Add remote agent install`

---

### Task 9: ZooKeeper ensemble across machines

**Files:** Modify `src/server/zookeeper.rs`, `src/mcp/remote_tools.rs`, `src/mcp/tools.rs` (`arcus_zk_start` accepts `members` and `my_ids`).

**Interfaces:**
- Produces:
  - `Node` gains `address: Option<String>` (`#[serde(default)]`); `zoo_cfg` writes `server.N=<address or 127.0.0.1>:<peer>:<election>` and, for a multi-machine ensemble, `quorumListenOnAllIPs=true`.
  - `arcus_zk_start` gains `members` (full node list with addresses) and `my_ids` (ids to run here); when given, only those nodes start locally with the shared member list; readiness waits for the local nodes to accept connections (not for quorum).
  - Tool `arcus_cluster_zk_plan {machines?: [...], nodes?: [...], version?}`: default plan = one node per given machine (ids 1.., ports 2181/2888/3888, address = that machine's first `hostname -I` address via `exec`); returns the plan only.
  - Tool `arcus_cluster_zk_start {nodes: [{id, machine, address, client_port, peer_port, election_port}], version?}`: forwards `arcus_zk_start {members, my_ids}` to each machine (download jobs are surfaced per machine), then polls each node's mode by forwarding `arcus_zk_mode {port}` to its machine, up to 60 s; returns `{ensemble: "addr:port,...", nodes: [{id, machine, mode}]}`; on timeout reports per-node state and, for nodes stuck without a mode, a hint that peer/election ports between machines may be blocked.
  - Tool `arcus_zk_mode {port}` (forwardable): returns `{mode: "leader"|"follower"|"standalone"|null}`.

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn cfg_with_addresses() { /* 3 nodes with addresses 10.0.0.1-3 → "server.2=10.0.0.2:2888:3888\n" and quorumListenOnAllIPs=true */ }
#[test] fn plan_one_node_per_machine() { /* fake runner returns "10.0.0.5 172.17.0.1" → address 10.0.0.5 */ }
#[test] fn cluster_start_reports_stuck_nodes() { /* fake forward: mode null for node 3 after timeout (inject short timeout) → isError mentions node 3 and blocked ports */ }
```

- [ ] **Step 2–4:** RED, implement, GREEN.
- [ ] **Step 5: Commit** `INTERNAL: Start a ZooKeeper ensemble across machines`

---

### Task 10: Cluster status, remote EE login consent, docs

**Files:** Modify `src/mcp/remote_tools.rs`, `src/mcp/tools.rs` (`arcus_ee_login`), `README.md`.

**Interfaces:**
- Produces:
  - Tool `arcus_cluster_status {machines?: [...]}`: forwards `arcus_status` to every registered (or given) machine in parallel threads; returns `{machines: [{machine, instances|error}]}`.
  - `arcus_ee_login` gains `accept_plaintext_token?: false`: when running on Linux with neither `DBUS_SESSION_BUS_ADDRESS` nor a `secret-tool` binary (no keyring), and the flag is false → isError explaining gh will store the token in `~/.config/gh/hosts.yml` (mode 600) and to ask the user before calling again with `accept_plaintext_token: true`.
  - README "Remote machines" section: ssh config reuse, `arcus_host_add/check/install`, service vs daemon and linger, the `machine` argument, cluster ZK, known limits (macOS exit status unknown after supervisor death; release tag requirement or `binary`).

- [ ] **Step 1: Write failing tests** — `cluster_status_runs_in_parallel` (fake forward with 300 ms delay × 3 machines finishes < 700 ms), `ee_login_requires_plaintext_consent_without_keyring`.
- [ ] **Step 2–4:** RED, implement, GREEN; `cargo test`, `cargo clippy` (no new warnings in `src/remote`, `src/mcp`, `src/server`).
- [ ] **Step 5: Commit** `INTERNAL: Add cluster status and remote EE login consent` and `DOC: Document remote machines`
- [ ] **Step 6: End-to-end (needs the user):** build a Linux aarch64 binary, then with the user present: `arcus_host_check dev-3` → `arcus_host_install {machine:"dev-3", binary, mode}` plan → apply → `arcus_install`/`arcus_setup`/`arcus_start` with `machine:"dev-3"`. Do not touch prd-* machines.
