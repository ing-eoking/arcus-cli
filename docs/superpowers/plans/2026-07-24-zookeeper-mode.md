# `--zookeeper` Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `--zookeeper` flag to arcus-cli that opens a zkCli-like REPL against a ZooKeeper ensemble, supporting `ls`/`get`/`create`/`set`/`delete`/`stat`, with command history disabled.

**Architecture:** A new isolated `src/zk/` module owns everything ZooKeeper: a pure `parse()` line-to-command function (the unit-tested surface), a `ZkClient` wrapping the `zookeeper` crate's connection, and a `run_repl()` that owns its own read-eval loop. `main.rs` branches to `zk::run_repl` when `--zookeeper` is set; the existing memcached TCP/UDP/Unix path is untouched.

**Tech Stack:** Rust (edition 2024), `zookeeper = "0.8"` (sync, pure-Rust), `clap`, `rustyline`.

## Global Constraints

- Rust edition: `2024` (already set in `Cargo.toml`).
- ZK backend: the synchronous `zookeeper = "0.8"` crate. No async/tokio.
- ZK-mode REPL must NOT record history: no `add_history_entry`, no `load_history`/`save_history`, independent of the `with-file-history` feature.
- ZK-mode REPL must NOT use `MyHelper` (no memcached hints).
- `create`: persistent node, `Acl::open_unsafe()` ACL. `set`/`delete`: version `None` (unconditional).
- Never panic on a ZK operation error — print to stderr and keep the REPL alive.
- Commit after each task.

## Verified crate API (`zookeeper` 0.8.0)

```rust
use zookeeper::{ZooKeeper, ZkResult, Acl, CreateMode, WatchedEvent, Stat};
use std::time::Duration;

// A closure `Fn(WatchedEvent) + Send` implements Watcher.
ZooKeeper::connect(connect_string: &str, timeout: Duration, watcher: W) -> ZkResult<ZooKeeper>
zk.get_children(path: &str, watch: bool) -> ZkResult<Vec<String>>
zk.get_data(path: &str, watch: bool) -> ZkResult<(Vec<u8>, Stat)>
zk.create(path: &str, data: Vec<u8>, acl: Vec<Acl>, mode: CreateMode) -> ZkResult<String>
zk.set_data(path: &str, data: Vec<u8>, version: Option<i32>) -> ZkResult<Stat>
zk.delete(path: &str, version: Option<i32>) -> ZkResult<()>
zk.exists(path: &str, watch: bool) -> ZkResult<Option<Stat>>
Acl::open_unsafe() -> &'static Vec<Acl>   // use .clone()
CreateMode::Persistent
// Stat fields: czxid, mzxid, ctime, mtime, version, cversion, aversion,
//              ephemeral_owner, data_length, num_children, pzxid
```

---

### Task 1: ZK command parser (`src/zk/mod.rs`)

Pure, I/O-free line parsing. This is the entire unit-tested surface.

**Files:**
- Create: `src/zk/mod.rs`
- Modify: `src/main.rs` (add `mod zk;` near the top, after `mod connect;`)

**Interfaces:**
- Produces: `pub enum ZkCommand { Ls(String), Get(String), Create(String, Vec<u8>), Set(String, Vec<u8>), Delete(String), Stat(String), Quit, Empty, Unknown(String) }`
- Produces: `pub fn parse(line: &str) -> ZkCommand`

Parsing rules:
- Trim the line; empty → `Empty`.
- Split on whitespace. First token = verb.
- `ls|get|delete|stat <path>` → require exactly 1 arg (the path); wrong arity → `Unknown` with a usage string.
- `create <path> [data...]` → 1 required arg + optional data; data tokens joined with a single space, `.into_bytes()`; missing path → `Unknown`.
- `set <path> <data...>` → path + at least 1 data token (joined with space); missing either → `Unknown`.
- `quit` → `Quit`.
- Any other verb → `Unknown(verb)`.

- [ ] **Step 1: Write the failing tests**

Add to `src/zk/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_line_is_empty() {
        assert!(matches!(parse("   "), ZkCommand::Empty));
        assert!(matches!(parse(""), ZkCommand::Empty));
    }

    #[test]
    fn ls_takes_a_path() {
        assert!(matches!(parse("ls /arcus"), ZkCommand::Ls(p) if p == "/arcus"));
    }

    #[test]
    fn ls_without_path_is_unknown() {
        assert!(matches!(parse("ls"), ZkCommand::Unknown(_)));
    }

    #[test]
    fn ls_with_extra_args_is_unknown() {
        assert!(matches!(parse("ls /a /b"), ZkCommand::Unknown(_)));
    }

    #[test]
    fn get_delete_stat_take_a_path() {
        assert!(matches!(parse("get /a"), ZkCommand::Get(p) if p == "/a"));
        assert!(matches!(parse("delete /a"), ZkCommand::Delete(p) if p == "/a"));
        assert!(matches!(parse("stat /a"), ZkCommand::Stat(p) if p == "/a"));
    }

    #[test]
    fn create_without_data_has_empty_bytes() {
        match parse("create /a") {
            ZkCommand::Create(p, d) => { assert_eq!(p, "/a"); assert!(d.is_empty()); }
            _ => panic!("expected Create"),
        }
    }

    #[test]
    fn create_joins_data_tokens_with_space() {
        match parse("create /a hello world") {
            ZkCommand::Create(p, d) => { assert_eq!(p, "/a"); assert_eq!(d, b"hello world"); }
            _ => panic!("expected Create"),
        }
    }

    #[test]
    fn create_without_path_is_unknown() {
        assert!(matches!(parse("create"), ZkCommand::Unknown(_)));
    }

    #[test]
    fn set_requires_path_and_data() {
        match parse("set /a val") {
            ZkCommand::Set(p, d) => { assert_eq!(p, "/a"); assert_eq!(d, b"val"); }
            _ => panic!("expected Set"),
        }
        assert!(matches!(parse("set /a"), ZkCommand::Unknown(_)));
    }

    #[test]
    fn quit_and_unknown() {
        assert!(matches!(parse("quit"), ZkCommand::Quit));
        assert!(matches!(parse("frobnicate /a"), ZkCommand::Unknown(_)));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path Cargo.toml zk::tests`
Expected: FAIL — `cannot find function parse` / `cannot find type ZkCommand`.

- [ ] **Step 3: Write the parser + enum**

At the top of `src/zk/mod.rs` (above the `tests` module):

```rust
#[derive(Debug)]
pub enum ZkCommand {
    Ls(String),
    Get(String),
    Create(String, Vec<u8>),
    Set(String, Vec<u8>),
    Delete(String),
    Stat(String),
    Quit,
    Empty,
    Unknown(String),
}

pub fn parse(line: &str) -> ZkCommand {
    let line = line.trim();
    if line.is_empty() {
        return ZkCommand::Empty;
    }

    let tokens: Vec<&str> = line.split_whitespace().collect();
    let verb = tokens[0];
    let args = &tokens[1..];

    match verb {
        "ls" if args.len() == 1 => ZkCommand::Ls(args[0].to_string()),
        "get" if args.len() == 1 => ZkCommand::Get(args[0].to_string()),
        "delete" if args.len() == 1 => ZkCommand::Delete(args[0].to_string()),
        "stat" if args.len() == 1 => ZkCommand::Stat(args[0].to_string()),
        "create" if args.len() >= 1 => {
            let data = args[1..].join(" ").into_bytes();
            ZkCommand::Create(args[0].to_string(), data)
        }
        "set" if args.len() >= 2 => {
            let data = args[1..].join(" ").into_bytes();
            ZkCommand::Set(args[0].to_string(), data)
        }
        "quit" => ZkCommand::Quit,
        "ls" | "get" | "delete" | "stat" | "create" | "set" => {
            ZkCommand::Unknown(format!("usage: {} requires a path (and data for set)", verb))
        }
        other => ZkCommand::Unknown(other.to_string()),
    }
}
```

Then add `mod zk;` to `src/main.rs` right after the existing `mod connect;` line.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path Cargo.toml zk::tests`
Expected: PASS (10 tests).

- [ ] **Step 5: Commit**

```bash
git add src/zk/mod.rs src/main.rs
git commit -m "INTERNAL: Add ZK command parser for arcus-cli zookeeper mode"
```

---

### Task 2: ZK client + REPL (`src/zk/mod.rs`)

Wraps the connection and drives the loop. No unit tests (needs a live server); verified manually in Task 3.

**Files:**
- Modify: `src/zk/mod.rs`
- Modify: `Cargo.toml` (add dependency)

**Interfaces:**
- Consumes: `parse()`, `ZkCommand` from Task 1.
- Produces: `pub fn run_repl(addr: &str, timeout: Duration) -> rustyline::Result<()>`

- [ ] **Step 1: Add the dependency**

Run: `cargo add zookeeper@0.8`
Expected: `Cargo.toml` `[dependencies]` gains `zookeeper = "0.8"`.

- [ ] **Step 2: Add imports and the client/REPL**

At the top of `src/zk/mod.rs`, above the `ZkCommand` enum:

```rust
use std::time::Duration;
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;
use zookeeper::{ZooKeeper, Acl, CreateMode, WatchedEvent};
```

Below `parse()` (above the `tests` module), add:

```rust
struct ZkClient {
    zk: ZooKeeper,
}

impl ZkClient {
    fn connect(addr: &str, timeout: Duration) -> zookeeper::ZkResult<ZkClient> {
        let zk = ZooKeeper::connect(addr, timeout, |ev: WatchedEvent| {
            eprintln!("WATCHER: {:?} state={:?} path={:?}",
                      ev.event_type, ev.keeper_state, ev.path);
        })?;
        Ok(ZkClient { zk })
    }

    /// Runs one command. Returns true when the loop should exit.
    fn execute(&self, cmd: ZkCommand) -> bool {
        match cmd {
            ZkCommand::Quit => return true,
            ZkCommand::Empty => {}
            ZkCommand::Unknown(msg) => eprintln!("ERROR: {}", msg),
            ZkCommand::Ls(path) => match self.zk.get_children(&path, false) {
                Ok(children) => println!("[{}]", children.join(", ")),
                Err(e) => eprintln!("ERROR: {:?}", e),
            },
            ZkCommand::Get(path) => match self.zk.get_data(&path, false) {
                Ok((data, _stat)) => println!("{}", String::from_utf8_lossy(&data)),
                Err(e) => eprintln!("ERROR: {:?}", e),
            },
            ZkCommand::Create(path, data) => {
                match self.zk.create(&path, data, Acl::open_unsafe().clone(), CreateMode::Persistent) {
                    Ok(created) => println!("Created {}", created),
                    Err(e) => eprintln!("ERROR: {:?}", e),
                }
            }
            ZkCommand::Set(path, data) => match self.zk.set_data(&path, data, None) {
                Ok(stat) => println!("version: {}", stat.version),
                Err(e) => eprintln!("ERROR: {:?}", e),
            },
            ZkCommand::Delete(path) => match self.zk.delete(&path, None) {
                Ok(()) => println!("Deleted {}", path),
                Err(e) => eprintln!("ERROR: {:?}", e),
            },
            ZkCommand::Stat(path) => match self.zk.exists(&path, false) {
                Ok(Some(stat)) => {
                    println!("czxid: {}", stat.czxid);
                    println!("mzxid: {}", stat.mzxid);
                    println!("ctime: {}", stat.ctime);
                    println!("mtime: {}", stat.mtime);
                    println!("version: {}", stat.version);
                    println!("cversion: {}", stat.cversion);
                    println!("aversion: {}", stat.aversion);
                    println!("ephemeralOwner: {}", stat.ephemeral_owner);
                    println!("dataLength: {}", stat.data_length);
                    println!("numChildren: {}", stat.num_children);
                    println!("pzxid: {}", stat.pzxid);
                }
                Ok(None) => eprintln!("ERROR: no such node: {}", path),
                Err(e) => eprintln!("ERROR: {:?}", e),
            },
        }
        false
    }
}

pub fn run_repl(addr: &str, timeout: Duration) -> rustyline::Result<()> {
    let client = match ZkClient::connect(addr, timeout) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ERROR: Failed to connect to ZooKeeper at {}: {:?}", addr, e);
            std::process::exit(1);
        }
    };

    // Fresh editor: no helper (no memcached hints), no history load/save.
    let mut rl = DefaultEditor::new()?;
    loop {
        match rl.readline("") {
            Ok(line) => {
                if client.execute(parse(&line)) {
                    break;
                }
            }
            Err(ReadlineError::Interrupted) | Err(ReadlineError::Eof) => break,
            Err(err) => {
                eprintln!("ERROR: {:?}", err);
                break;
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 3: Build to verify it compiles**

Run: `cargo build`
Expected: builds successfully. `run_repl` may be flagged unused (`warning: function is never used`) — that is expected until Task 3 calls it.

- [ ] **Step 4: Confirm existing tests still pass**

Run: `cargo test`
Expected: PASS (the 10 parser tests; no regressions).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/zk/mod.rs
git commit -m "INTERNAL: Add ZooKeeper client and REPL loop for arcus-cli zookeeper mode"
```

---

### Task 3: CLI wiring + README

Add the flag, resolve the port default, branch in `main`, document it.

**Files:**
- Modify: `src/main.rs`
- Modify: `README.md`

**Interfaces:**
- Consumes: `zk::run_repl` from Task 2.

- [ ] **Step 1: Add the `--zookeeper` flag and make `--port` optional**

In `src/main.rs`, in `struct Args`, change the `port` field and add the flag:

```rust
    /// Port Number (default: 11211, or 2181 in --zookeeper mode)
    #[arg(short, long)]
    port: Option<u16>,

    /// Connect to a ZooKeeper ensemble (zkCli-like), disables memcached mode
    #[clap(long, action=ArgAction::SetTrue)]
    zookeeper: bool,
```

(Leave the other fields unchanged. `port` is now `Option<u16>`.)

- [ ] **Step 2: Resolve the port and branch to the ZK REPL in `main`**

In `src/main.rs`, immediately after `let args = Args::parse();`, add the port resolution:

```rust
    let default_port = if args.zookeeper { 2181 } else { 11211 };
    let port = args.port.unwrap_or(default_port);
```

Then in the block that builds `addr`, replace `args.port` with the resolved `port`. The non-unix branch becomes:

```rust
    let addr = if args.unix {
        args.host
    } else {
        format!("{}:{}", args.host, port)
    };
```

Add the ZK branch right after `addr` is built and before the `Transport` builder / memcached loop:

```rust
    if args.zookeeper {
        return zk::run_repl(&addr, timeout);
    }
```

(`timeout` is already defined above as `time::Duration::from_micros(args.timeout)`.)

- [ ] **Step 3: Build and verify no warnings about unused `run_repl`**

Run: `cargo build`
Expected: builds successfully; the earlier "function is never used" warning for `run_repl` is gone.

- [ ] **Step 4: Verify tests + CLI help**

Run: `cargo test && cargo run -- --help`
Expected: tests PASS; `--help` output lists `--zookeeper` and shows `--port` as optional.

- [ ] **Step 5: Manual verification against a live ZooKeeper**

Requires a local ZooKeeper on `127.0.0.1:2181`. Run:

```bash
cargo run -- --zookeeper --host 127.0.0.1
```

Then at the prompt, exercise each command and confirm the output:

```
ls /
create /arcus_cli_test hello
get /arcus_cli_test        # -> hello
set /arcus_cli_test world
get /arcus_cli_test        # -> world
stat /arcus_cli_test       # -> czxid/version/... fields
ls /                       # -> list includes arcus_cli_test
delete /arcus_cli_test
get /arcus_cli_test        # -> ERROR: ... NoNode
quit
```

Also confirm: pressing Up-arrow shows NO previous-command history, and no memcached hints appear while typing.

- [ ] **Step 6: Update the README**

In `README.md`, under `## Features`, add a bullet `- **ZooKeeper Mode (zkCli-like)**`. Under `## Execution`, add:

````markdown
```bash
# Connect to a ZooKeeper ensemble (like zkCli); history is disabled
cargo run -- --zookeeper --host 127.0.0.1 --port 2181
```

In ZooKeeper mode the REPL supports: `ls <path>`, `get <path>`,
`create <path> [data]`, `set <path> <data>`, `delete <path>`, `stat <path>`,
and `quit`.
````

- [ ] **Step 7: Commit**

```bash
git add src/main.rs README.md
git commit -m "INTERNAL: Wire --zookeeper flag into arcus-cli CLI and docs"
```

---

## Self-Review Notes

- **Spec coverage:** crate choice (Task 2 dep), `--zookeeper` flag + `Option<u16>` port resolution to 2181/11211 (Task 3), isolated `src/zk` module (Tasks 1–2), single branch in `main` (Task 3), history-off + no-helper editor (Task 2 `run_repl`), all six commands + quit (Tasks 1–2), create=persistent+open-unsafe / set&delete version None (Task 2), connect-fails-exit + per-command errors stay alive + watcher logging (Task 2), parser TDD (Task 1), manual live verification + README (Task 3). All covered.
- **Types:** `ZkCommand`, `parse`, `run_repl` signatures are consistent across tasks and match the verified crate API.
