# Design: `--zookeeper` mode for arcus-cli

Date: 2026-07-24

## Goal

Add a `--zookeeper` flag to arcus-cli. When set, the tool connects to a
ZooKeeper ensemble (like `zkCli`) instead of a memcached server, and offers an
interactive REPL for browsing and editing znodes (`ls`, `get`, `create`, `set`,
`delete`, `stat`). Command history is disabled in this mode.

## Non-goals (YAGNI)

- Ephemeral / sequential node flags (`-e` / `-s`) on `create`.
- ACL management (`getAcl` / `setAcl`).
- `deleteall` / recursive delete, `ls2`, `sync`.
- ZooKeeper-specific autocompletion/hints in the REPL.
- Automated integration tests against a live ZooKeeper server.

## Backend decision

Use the synchronous, pure-Rust [`zookeeper`](https://crates.io/crates/zookeeper)
crate (`0.8`). The existing codebase is fully synchronous (std threads, blocking
sockets, no tokio); the async `zookeeper-client` / `tokio-zookeeper` crates would
require pulling in a tokio runtime for no benefit here. The `zookeeper` crate
provides `get_children`, `get_data`, `create`, `set_data`, `delete`, and
`exists`, which cover the full command set.

`Cargo.toml` gains: `zookeeper = "0.8"`.

## CLI surface

- New flag `--zookeeper` (bool). Conceptually mutually exclusive with `--udp`,
  `--unix`, and `--sasl`; when `--zookeeper` is set it takes priority and those
  memcached-only options are ignored.
- `--port` changes from `u16` (default `11211`) to `Option<u16>`. The default is
  resolved at runtime:
  - ZK mode: `2181`
  - otherwise: `11211`

  So `arcus-cli --zookeeper --host 127.0.0.1` connects to `127.0.0.1:2181`, and
  `--port 2181` remains optional.
- The connect string passed to the ZK client is `"{host}:{port}"`.

## Module layout

ZooKeeper is request/response with structured results and does not fit the
existing line-oriented `Connection` trait (`connect`/`write`/`close`). So it
lives in its own isolated module rather than as a new `Transport` variant.

New file `src/zk/mod.rs`, declared as `mod zk;` in `main.rs`. It contains:

- `enum ZkCommand` — the parsed command:
  `Ls(String)`, `Get(String)`, `Create(String, Vec<u8>)`, `Set(String, Vec<u8>)`,
  `Delete(String)`, `Stat(String)`, `Quit`, `Empty`, `Unknown(String)`.
- `fn parse(line: &str) -> ZkCommand` — pure, no I/O. Splits the line, maps the
  first token to a command, validates argument count (returns `Unknown` with a
  usage hint on arity errors). This is the primary unit-tested surface.
- `struct ZkClient { zk: ZooKeeper }` with:
  - `fn connect(addr: &str, timeout: Duration) -> zookeeper::ZkResult<ZkClient>`
  - `fn execute(&self, cmd: ZkCommand)` — runs the op, prints the result or an
    error to stderr; never panics, never exits (except it signals quit to the
    caller).
- `fn run_repl(addr: &str, timeout: Duration) -> rustyline::Result<()>` — owns
  the read-eval loop for ZK mode.

## Control flow in `main.rs`

`main()` branches once, early:

```
if args.zookeeper {
    return zk::run_repl(&addr, timeout);
}
// ... existing memcached Editor + Transport loop, unchanged ...
```

The existing TCP/UDP/Unix path and its `MyHelper`/history handling are left
untouched.

## REPL behavior in ZK mode

Differs deliberately from the memcached REPL:

- **History off.** A fresh `rustyline::Editor` with no `add_history_entry`
  calls and no `load_history`/`save_history` — independent of the
  `with-file-history` feature.
- **No memcached hints.** The `Editor` is created without `MyHelper`, so none of
  the memcached command hints appear.
- Prompt reads a line, `parse()`s it, and `execute()`s it. `quit` (and EOF /
  Ctrl-C) exit the loop.

## Command semantics

| input                    | ZK op                                          | success output              |
|--------------------------|------------------------------------------------|-----------------------------|
| `ls <path>`              | `get_children(path, false)`                    | newline/space list of names |
| `get <path>`             | `get_data(path, false)`                        | node data as UTF-8 (lossy)  |
| `create <path> [data]`   | `create(path, data, OPEN_ACL_UNSAFE, Persistent)` | `Created <path>`         |
| `set <path> <data>`      | `set_data(path, data, version=-1)`             | new version number          |
| `delete <path>`          | `delete(path, version=-1)`                     | `Deleted <path>`            |
| `stat <path>`            | `exists(path, false)`                          | Stat fields, or "no such node" |

- `create` with no data creates a node with empty data.
- `create` uses persistent nodes and the open/unsafe ACL — matching zkCli's
  default `create` with no flags.
- `set` / `delete` use version `-1` (unconditional), matching zkCli defaults.

## Error handling

- `ZkClient::connect` failure: print a warning and exit (no session to run a REPL
  against) — cannot follow the memcached "reconnect lazily" pattern because there
  is no meaningful REPL without a session.
- Per-command ZK errors (`NoNode`, `NodeExists`, `BadArguments`, etc.): print to
  stderr; the REPL stays alive. Same resilience philosophy as the memcached loop.
- A connection watcher (`|event|`) logs session/keeper state changes to stderr.

## Testing

- **TDD the parser.** `parse()` is pure and gets unit tests in `src/zk/mod.rs`:
  each command with valid args, arity errors → `Unknown`, empty line → `Empty`,
  `create` with and without data, data tokens joined correctly, unknown verb.
- **Manual verification** of live ls/create/get/set/delete/stat against a real
  ZooKeeper ensemble, documented as a step in the README. Not automated (needs an
  external server).

## README

Add a ZooKeeper mode section with an example:

```bash
# Connect to a ZooKeeper ensemble (like zkCli)
cargo run -- --zookeeper --host 127.0.0.1 --port 2181
```
