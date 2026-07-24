# ZK Recursive `ls` + Tab Completion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `ls -R`/`-s` flags and Tab autocompletion (commands + live znode paths) to arcus-cli's `--zookeeper` REPL.

**Architecture:** Two pure, unit-tested functions (`parse()` extended for `ls` flags; a new `completion_target()`) plus live-ZK behavior in `execute()` and a new rustyline `Completer`. The single `ZooKeeper` session becomes `Rc<ZooKeeper>`, shared between `ZkClient` and a new `ZkHelper` so completion can query real children.

**Tech Stack:** Rust (edition 2024), `zookeeper = "0.8"`, `rustyline = "18"`, `rustyline-derive = "0.12"`.

## Global Constraints

- All work is in `src/zk/mod.rs` (except README). Do NOT touch `main.rs`, `src/connect/`, or `src/helper/`.
- History stays disabled in ZK mode: no `add_history_entry`, no `load_history`/`save_history`. `ZkHelper`'s `Hinter`/`Highlighter`/`Validator` must be no-op derives (no memcached-style hints).
- The ZK session is single-threaded (one REPL thread): use `Rc<ZooKeeper>`, not `Arc`. Do not spawn threads.
- Completion must never error the editor: on any ZK error during completion, return an empty candidate list.
- Paths are absolute; completion only engages for tokens starting with `/`.
- Command list for completion (exact, this order): `["ls", "get", "create", "set", "delete", "stat", "quit"]`.
- Never panic on a ZK operation error — print to stderr, keep the REPL alive.
- `Cargo.lock` is gitignored in this repo — do not force-add it.
- Follow TDD for the pure functions (RED then GREEN). Commit after each task.

## Verified API

```rust
// rustyline 18
use rustyline::completion::{Completer, Pair};   // Pair { display: String, replacement: String }
use rustyline::{Editor, Context};
use rustyline::history::DefaultHistory;
use rustyline_derive::{Helper, Hinter, Highlighter, Validator};
// trait: fn complete(&self, line: &str, pos: usize, ctx: &Context<'_>)
//            -> rustyline::Result<(usize, Vec<Self::Candidate>)>;
// Candidate is implemented for Pair and for String.

// zookeeper 0.8
zk.get_children(path: &str, watch: bool) -> ZkResult<Vec<String>>
zk.exists(path: &str, watch: bool) -> ZkResult<Option<Stat>>
```

Current `src/zk/mod.rs` (before this plan): `ZkCommand` enum with `Ls(String)`; `parse()`; `struct ZkClient { zk: ZooKeeper }` with `connect`/`execute`; `run_repl` using `DefaultEditor`. Tasks below modify these.

---

### Task 1: Extend `ls` parsing with `-R` / `-s` flags

**Files:**
- Modify: `src/zk/mod.rs` (the `ZkCommand` enum, `parse()`, and the `ls` tests)

**Interfaces:**
- Produces: `ZkCommand::Ls { path: String, recursive: bool, stat: bool }` (replaces `Ls(String)`).
- Produces: `parse()` handling `-R`/`-s` for `ls`.

- [ ] **Step 1: Update the failing tests**

In `src/zk/mod.rs`, replace the existing `ls_takes_a_path`, `ls_without_path_is_unknown`, and `ls_with_extra_args_is_unknown` tests with:

```rust
    #[test]
    fn ls_plain_has_no_flags() {
        match parse("ls /arcus") {
            ZkCommand::Ls { path, recursive, stat } => {
                assert_eq!(path, "/arcus"); assert!(!recursive); assert!(!stat);
            }
            _ => panic!("expected Ls"),
        }
    }

    #[test]
    fn ls_recursive_flag() {
        match parse("ls -R /arcus") {
            ZkCommand::Ls { path, recursive, stat } => {
                assert_eq!(path, "/arcus"); assert!(recursive); assert!(!stat);
            }
            _ => panic!("expected Ls"),
        }
    }

    #[test]
    fn ls_stat_flag() {
        match parse("ls -s /arcus") {
            ZkCommand::Ls { path, recursive, stat } => {
                assert_eq!(path, "/arcus"); assert!(!recursive); assert!(stat);
            }
            _ => panic!("expected Ls"),
        }
    }

    #[test]
    fn ls_both_flags_any_order() {
        for line in ["ls -R -s /a", "ls -s -R /a", "ls /a -R -s", "ls -R /a -s"] {
            match parse(line) {
                ZkCommand::Ls { path, recursive, stat } => {
                    assert_eq!(path, "/a", "line: {line}");
                    assert!(recursive, "line: {line}");
                    assert!(stat, "line: {line}");
                }
                _ => panic!("expected Ls for {line}"),
            }
        }
    }

    #[test]
    fn ls_without_path_is_unknown() {
        assert!(matches!(parse("ls"), ZkCommand::Unknown(_)));
        assert!(matches!(parse("ls -R"), ZkCommand::Unknown(_)));
    }

    #[test]
    fn ls_with_two_paths_is_unknown() {
        assert!(matches!(parse("ls /a /b"), ZkCommand::Unknown(_)));
    }

    #[test]
    fn ls_with_unknown_flag_is_unknown() {
        assert!(matches!(parse("ls -x /a"), ZkCommand::Unknown(_)));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path Cargo.toml zk::tests`
Expected: FAIL — the `Ls` struct-variant pattern doesn't compile / old `Ls(String)` mismatch.

- [ ] **Step 3: Update the enum and parser**

In `src/zk/mod.rs`, change the enum variant:

```rust
    Ls { path: String, recursive: bool, stat: bool },
```

Replace the `ls` arm(s) in `parse()`. Remove `ls` from both the single-arg arm and the shared `"ls" | "get" | ...` usage arm, and add a dedicated block. The `match verb` becomes:

```rust
    match verb {
        "ls" => parse_ls(args),
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
        "get" | "delete" | "stat" | "create" | "set" => {
            ZkCommand::Unknown(format!("usage: {} requires a path (and data for set)", verb))
        }
        other => ZkCommand::Unknown(other.to_string()),
    }
```

Add this free function directly below `parse()`:

```rust
fn parse_ls(args: &[&str]) -> ZkCommand {
    let mut recursive = false;
    let mut stat = false;
    let mut path: Option<&str> = None;
    for tok in args {
        match *tok {
            "-R" => recursive = true,
            "-s" => stat = true,
            t if t.starts_with('-') => {
                return ZkCommand::Unknown(format!("ls: unknown flag {}", t));
            }
            t => {
                if path.is_some() {
                    return ZkCommand::Unknown("usage: ls [-R] [-s] <path>".to_string());
                }
                path = Some(t);
            }
        }
    }
    match path {
        Some(p) => ZkCommand::Ls { path: p.to_string(), recursive, stat },
        None => ZkCommand::Unknown("usage: ls [-R] [-s] <path>".to_string()),
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path Cargo.toml zk::tests`
Expected: FAIL to COMPILE only in `execute()` — the `ZkCommand::Ls(path)` arm there no longer matches the struct variant. That is fixed in Task 2. To confirm the PARSER logic in isolation for now, this task's compile will break at `execute`. So instead, temporarily verify by updating the `execute` match arm in THIS task as part of Step 3 as well:

In `execute()`, change the existing arm

```rust
            ZkCommand::Ls(path) => match self.zk.get_children(&path, false) {
                Ok(children) => println!("[{}]", children.join(", ")),
                Err(e) => eprintln!("ERROR: {:?}", e),
            },
```

to (placeholder — full flag behavior lands in Task 2):

```rust
            ZkCommand::Ls { path, recursive: _, stat: _ } => match self.zk.get_children(&path, false) {
                Ok(children) => println!("[{}]", children.join(", ")),
                Err(e) => eprintln!("ERROR: {:?}", e),
            },
```

Then re-run: `cargo test`
Expected: PASS (all parser tests including the new `ls` ones; existing tests still green).

- [ ] **Step 5: Commit**

```bash
git add src/zk/mod.rs
git commit -m "INTERNAL: Parse -R and -s flags for ZK ls command"
```

---

### Task 2: Recursive / stat `ls` execution

**Files:**
- Modify: `src/zk/mod.rs` (`execute()` `Ls` arm + a stat-printing helper + a recursive walk helper)

**Interfaces:**
- Consumes: `ZkCommand::Ls { path, recursive, stat }` from Task 1.
- Produces: private `fn print_stat(&self, path: &str)` and `fn ls_recursive(&self, path: &str)` on `ZkClient`; a free `fn join_zk_path(parent: &str, child: &str) -> String`.

- [ ] **Step 1: Write the failing test for path joining**

`join_zk_path` is the one pure piece worth a test. Add to the `tests` module:

```rust
    #[test]
    fn join_zk_path_root_and_nested() {
        assert_eq!(join_zk_path("/", "arcus"), "/arcus");
        assert_eq!(join_zk_path("/arcus", "cache_list"), "/arcus/cache_list");
        assert_eq!(join_zk_path("/a/b", "c"), "/a/b/c");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path Cargo.toml zk::tests::join_zk_path_root_and_nested`
Expected: FAIL — `cannot find function join_zk_path`.

- [ ] **Step 3: Implement joining, stat printing, recursive walk, and the full `Ls` arm**

Add the free function near the top of the file (below `parse_ls`):

```rust
fn join_zk_path(parent: &str, child: &str) -> String {
    if parent == "/" {
        format!("/{}", child)
    } else {
        format!("{}/{}", parent, child)
    }
}
```

Extract the stat printing (currently inline in the `Stat` arm) into a method so `ls -s` can reuse it. Add to `impl ZkClient`:

```rust
    fn print_stat(&self, stat: &zookeeper::Stat) {
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

    fn ls_recursive(&self, path: &str) {
        println!("{}", path);
        match self.zk.get_children(path, false) {
            Ok(children) => {
                for child in children {
                    self.ls_recursive(&join_zk_path(path, &child));
                }
            }
            Err(e) => eprintln!("ERROR: {}: {:?}", path, e),
        }
    }
```

Update the existing `Stat` arm to use `print_stat`:

```rust
            ZkCommand::Stat(path) => match self.zk.exists(&path, false) {
                Ok(Some(stat)) => self.print_stat(&stat),
                Ok(None) => eprintln!("ERROR: no such node: {}", path),
                Err(e) => eprintln!("ERROR: {:?}", e),
            },
```

Replace the placeholder `Ls` arm from Task 1 with the full behavior:

```rust
            ZkCommand::Ls { path, recursive, stat } => {
                if recursive {
                    self.ls_recursive(&path);
                } else {
                    match self.zk.get_children(&path, false) {
                        Ok(children) => println!("[{}]", children.join(", ")),
                        Err(e) => eprintln!("ERROR: {:?}", e),
                    }
                }
                if stat {
                    match self.zk.exists(&path, false) {
                        Ok(Some(s)) => self.print_stat(&s),
                        Ok(None) => eprintln!("ERROR: no such node: {}", path),
                        Err(e) => eprintln!("ERROR: {:?}", e),
                    }
                }
            }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test`
Expected: PASS (parser tests + `join_zk_path_root_and_nested`; no regressions).

- [ ] **Step 5: Build to confirm no warnings**

Run: `cargo build`
Expected: compiles; only the known upstream `zookeeper` future-incompat note is acceptable.

- [ ] **Step 6: Commit**

```bash
git add src/zk/mod.rs
git commit -m "INTERNAL: Implement recursive and stat ls for ZK mode"
```

---

### Task 3: `completion_target()` — pure completion classifier

**Files:**
- Modify: `src/zk/mod.rs` (add `CompletionTarget` enum, `completion_target()`, and tests)

**Interfaces:**
- Produces: `enum CompletionTarget { Command { start: usize, prefix: String }, Path { start: usize, parent: String, prefix: String }, None }` and `fn completion_target(line: &str, pos: usize) -> CompletionTarget`. Both used by Task 4.

- [ ] **Step 1: Write the failing tests**

Add a second test module block (or extend `tests`) in `src/zk/mod.rs`:

```rust
    #[test]
    fn completion_first_token_is_command() {
        match completion_target("l", 1) {
            CompletionTarget::Command { start, prefix } => { assert_eq!(start, 0); assert_eq!(prefix, "l"); }
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn completion_empty_line_is_command_all() {
        match completion_target("", 0) {
            CompletionTarget::Command { start, prefix } => { assert_eq!(start, 0); assert_eq!(prefix, ""); }
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn completion_path_at_root() {
        match completion_target("ls /arc", 7) {
            CompletionTarget::Path { start, parent, prefix } => {
                assert_eq!(start, 4);          // just after the leading '/'
                assert_eq!(parent, "/");
                assert_eq!(prefix, "arc");
            }
            _ => panic!("expected Path"),
        }
    }

    #[test]
    fn completion_path_trailing_slash() {
        match completion_target("get /arcus/", 11) {
            CompletionTarget::Path { start, parent, prefix } => {
                assert_eq!(start, 11);
                assert_eq!(parent, "/arcus");
                assert_eq!(prefix, "");
            }
            _ => panic!("expected Path"),
        }
    }

    #[test]
    fn completion_path_nested_prefix() {
        match completion_target("get /arcus/ca", 13) {
            CompletionTarget::Path { start, parent, prefix } => {
                assert_eq!(start, 11);
                assert_eq!(parent, "/arcus");
                assert_eq!(prefix, "ca");
            }
            _ => panic!("expected Path"),
        }
    }

    #[test]
    fn completion_non_path_arg_is_none() {
        assert!(matches!(completion_target("get foo", 7), CompletionTarget::None));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path Cargo.toml zk::tests`
Expected: FAIL — `cannot find CompletionTarget` / `completion_target`.

- [ ] **Step 3: Implement the classifier**

Add near the top of `src/zk/mod.rs` (below `parse_ls`):

```rust
#[derive(Debug, PartialEq)]
pub enum CompletionTarget {
    Command { start: usize, prefix: String },
    Path { start: usize, parent: String, prefix: String },
    None,
}

pub fn completion_target(line: &str, pos: usize) -> CompletionTarget {
    let head = &line[..pos];
    let tok_start = head.rfind(char::is_whitespace).map(|i| i + 1).unwrap_or(0);
    let tok = &head[tok_start..];

    let before_tok = &head[..tok_start];
    if before_tok.trim().is_empty() {
        return CompletionTarget::Command { start: tok_start, prefix: tok.to_string() };
    }

    if !tok.starts_with('/') {
        return CompletionTarget::None;
    }

    let slash = tok.rfind('/').unwrap();
    let prefix = tok[slash + 1..].to_string();
    let parent = if slash == 0 { "/".to_string() } else { tok[..slash].to_string() };
    let start = tok_start + slash + 1;
    CompletionTarget::Path { start, parent, prefix }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path Cargo.toml zk::tests`
Expected: PASS (all completion_target tests + prior tests).

- [ ] **Step 5: Commit**

```bash
git add src/zk/mod.rs
git commit -m "INTERNAL: Add pure completion classifier for ZK REPL"
```

---

### Task 4: `ZkHelper` completer + share the connection via `Rc`

**Files:**
- Modify: `src/zk/mod.rs` (imports, `ZkClient` field type, `ZkHelper`, `run_repl`)

**Interfaces:**
- Consumes: `completion_target()` and the `CMDS` command list.
- Produces: `struct ZkHelper` implementing `Completer`; `ZkClient` and `run_repl` using `Rc<ZooKeeper>`.

No new unit tests (completion queries need a live server; verified manually). This task is build + existing-tests-green + manual live check by the controller.

- [ ] **Step 1: Update imports and connection sharing**

At the top of `src/zk/mod.rs`, extend imports:

```rust
use std::rc::Rc;
use std::time::Duration;
use rustyline::{Editor, Context};
use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::history::DefaultHistory;
use rustyline_derive::{Helper, Hinter, Highlighter, Validator};
use zookeeper::{ZooKeeper, Acl, CreateMode, WatchedEvent};
```

Change `ZkClient` to hold a shared handle:

```rust
struct ZkClient {
    zk: Rc<ZooKeeper>,
}
```

And update `connect` to store the `Rc` (build it from a shared handle passed in). Replace the `connect` method with a constructor that takes an existing `Rc<ZooKeeper>`:

```rust
impl ZkClient {
    fn new(zk: Rc<ZooKeeper>) -> ZkClient {
        ZkClient { zk }
    }
```

(Keep the rest of `impl ZkClient` — `execute`, `print_stat`, `ls_recursive` — unchanged; they call `self.zk.<method>` which still works through the `Rc` deref.)

- [ ] **Step 2: Add the `CMDS` list and `ZkHelper`**

Add near the top (below the enum):

```rust
const CMDS: [&str; 7] = ["ls", "get", "create", "set", "delete", "stat", "quit"];
```

Add the helper (place it above `run_repl`):

```rust
#[derive(Helper, Hinter, Highlighter, Validator)]
struct ZkHelper {
    zk: Rc<ZooKeeper>,
}

impl Completer for ZkHelper {
    type Candidate = Pair;

    fn complete(&self, line: &str, pos: usize, _ctx: &Context<'_>)
        -> rustyline::Result<(usize, Vec<Pair>)>
    {
        match completion_target(line, pos) {
            CompletionTarget::Command { start, prefix } => {
                let pairs = CMDS.iter()
                    .filter(|c| c.starts_with(&prefix))
                    .map(|c| Pair { display: c.to_string(), replacement: c.to_string() })
                    .collect();
                Ok((start, pairs))
            }
            CompletionTarget::Path { start, parent, prefix } => {
                let pairs = match self.zk.get_children(&parent, false) {
                    Ok(children) => children.into_iter()
                        .filter(|name| name.starts_with(&prefix))
                        .map(|name| Pair { display: name.clone(), replacement: name })
                        .collect(),
                    Err(_) => Vec::new(),
                };
                Ok((start, pairs))
            }
            CompletionTarget::None => Ok((pos, Vec::new())),
        }
    }
}
```

- [ ] **Step 3: Rewrite `run_repl` to connect once, share, and use the helper**

Replace the whole `run_repl` function with:

```rust
pub fn run_repl(addr: &str, timeout: Duration) -> rustyline::Result<()> {
    let zk = match ZooKeeper::connect(addr, timeout, |ev: WatchedEvent| {
        eprintln!("WATCHER: {:?} state={:?} path={:?}",
                  ev.event_type, ev.keeper_state, ev.path);
    }) {
        Ok(zk) => Rc::new(zk),
        Err(e) => {
            eprintln!("ERROR: Failed to connect to ZooKeeper at {}: {:?}", addr, e);
            std::process::exit(1);
        }
    };

    let client = ZkClient::new(Rc::clone(&zk));

    // Editor with completion only: no history load/save, no hints.
    let mut rl: Editor<ZkHelper, DefaultHistory> = Editor::new()?;
    rl.set_helper(Some(ZkHelper { zk: Rc::clone(&zk) }));

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

Note: the old `ZkClient::connect` method is removed (replaced by `new`); ensure no other caller references it (only `run_repl` did).

- [ ] **Step 4: Build and run existing tests**

Run: `cargo build && cargo test`
Expected: compiles (only upstream `zookeeper` future-incompat note); all unit tests pass. No "unused" warnings for `ZkHelper`/`completion_target`/`CMDS`.

- [ ] **Step 5: Manual live verification (controller)**

Requires a local ZooKeeper on 127.0.0.1:2181. Build, then:

```bash
cargo run -- --zookeeper --host 127.0.0.1
```

Confirm, in an interactive terminal:
- `ls -R /` prints one absolute path per line, DFS, starting at `/`.
- `ls -s /` prints the `[...]` list then a Stat block.
- `ls -R -s /arcus` prints the recursive listing then one Stat block of `/arcus`.
- Typing `cr` then Tab completes to `create`.
- Typing `ls /` then Tab lists root children; `ls /ar` + Tab completes to `/arcus` (if it exists).
- Up-arrow shows NO history (still disabled).

(Non-interactive piped input cannot exercise Tab; the controller performs the recursive/stat checks via a piped script and the Tab checks interactively or documents them as construction-guaranteed.)

- [ ] **Step 6: Commit**

```bash
git add src/zk/mod.rs
git commit -m "INTERNAL: Add Tab completion for commands and znode paths in ZK REPL"
```

---

### Task 5: README

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Update the ZooKeeper mode section**

In `README.md`, change the ZooKeeper command-list paragraph to document the flags and Tab completion. Replace:

```
In ZooKeeper mode the REPL supports: `ls <path>`, `get <path>`,
`create <path> [data]`, `set <path> <data>`, `delete <path>`, `stat <path>`,
and `quit`.
```

with:

```
In ZooKeeper mode the REPL supports: `ls [-R] [-s] <path>` (`-R` recursive,
`-s` with stat), `get <path>`, `create <path> [data]`, `set <path> <data>`,
`delete <path>`, `stat <path>`, and `quit`. Press Tab to complete command
names and existing znode paths at the current path.
```

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "DOC: Document ZK ls flags and Tab completion"
```

---

## Self-Review Notes

- **Spec coverage:** `ls` `-R`/`-s`/combo parsing (Task 1), recursive+stat execution with the `-R -s` = listing + single top stat semantics and mid-walk error skip (Task 2), pure `completion_target` classifier incl. root/trailing-slash/non-path/empty (Task 3), `ZkHelper` Completer + command list + live path children + never-error + `Rc` sharing + history-off/no-hints (Task 4), README (Task 5). All covered.
- **Types:** `ZkCommand::Ls { path, recursive, stat }`, `CompletionTarget`, `completion_target`, `CMDS`, `ZkHelper`, `Rc<ZooKeeper>`, `Pair` used consistently across tasks and match the verified APIs.
- **Placeholder note:** Task 1 Step 4 intentionally patches the `execute` `Ls` arm to a placeholder so the crate compiles at the end of Task 1; Task 2 replaces it with full behavior. Task 3's Step 3 explicitly instructs writing the final classifier (the second code block), not the guarded draft.
