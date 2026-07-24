# Design: recursive `ls` + tab completion for arcus-cli ZooKeeper mode

Date: 2026-07-24

## Goal

Extend arcus-cli's `--zookeeper` mode (see
`2026-07-24-zookeeper-mode-design.md`) with two zkCli-like conveniences:

1. `ls` flags `-R` (recursive) and `-s` (stat), including the `-R -s` combo.
2. Tab-key autocompletion of both command names and znode paths, where paths
   complete from the real children that exist at the typed path.

## Non-goals (YAGNI)

- The `-w` (watch) flag on `ls`.
- Flag autocompletion (completing `-R`/`-s` after `ls`).
- Relative-path completion (paths must be absolute, starting with `/`).
- A `cd`/current-working-path concept — the REPL stays stateless.
- Data-value or field-name completion.

## 1. Command model change

`ZkCommand::Ls(String)` becomes:

```rust
Ls { path: String, recursive: bool, stat: bool }
```

`parse()` for the `ls` verb:
- Tokens after `ls` are classified: `-R` sets `recursive`, `-s` sets `stat`,
  any other token is a path candidate.
- Exactly one path candidate is required.
- Zero paths, more than one path, or an unrecognized `-x` flag → `Unknown`
  with a usage string.
- Flags may appear in any order, before or after the path
  (`ls -R -s /a`, `ls /a -R`, `ls -s /a` all valid).
- `ls /a` → `Ls { path: "/a", recursive: false, stat: false }` (unchanged
  behavior).

Other commands (`get`/`create`/`set`/`delete`/`stat`/`quit`) are unchanged.

## 2. `ls` execution semantics (zkCli-like)

Given `Ls { path, recursive, stat }`:

- **Neither flag** — `get_children(path)`, print `[a, b, c]` (as today).
- **`-s` only** — print the `[a, b, c]` children list, then the Stat block of
  `path` (same 11 fields as the `stat` command).
- **`-R` only** — DFS pre-order walk starting at `path`; print one absolute
  path per line, `path` itself first, then each descendant.
- **`-R -s`** — the recursive listing, then a single Stat block of `path` at
  the end (NOT a stat per node).

Recursive walk:
- Child full path = `format!("/{}", child)` when `path == "/"`, else
  `format!("{}/{}", path, child)`.
- If `get_children` on a node fails mid-walk (e.g. concurrent delete), print
  the error to stderr and skip that subtree — do not abort the whole listing.
- Order: for each node, print the node, then recurse into its children in the
  order ZooKeeper returns them.

If the top `path` does not exist, `get_children` errors (`NoNode`) → print the
error, print nothing else. This matches the existing per-command error
behavior (stderr, REPL stays alive).

## 3. Tab completion

### Editor / helper

`run_repl` replaces `DefaultEditor` with `Editor<ZkHelper, DefaultHistory>`:

- `ZkHelper` derives `Helper`, `Hinter`, `Highlighter`, `Validator` (all
  no-ops via `rustyline_derive`) and hand-implements `Completer`
  (`type Candidate = rustyline::completion::Pair`).
- History stays disabled: no `add_history_entry`, no `load_history`/
  `save_history`. The helper adds ONLY completion — no hints, matching the
  "no memcached hints" spirit of ZK mode.

### Shared connection

The single `ZooKeeper` session is wrapped in `Rc<ZooKeeper>`:
- `ZkClient` holds `Rc<ZooKeeper>`.
- `ZkHelper` holds a clone of the same `Rc<ZooKeeper>`.

Both live on the one REPL thread, so `Rc` (not `Arc`) is sufficient — no extra
threads are introduced. `run_repl` connects once, then builds both from the
shared handle.

### Classification (pure, unit-tested)

```rust
enum CompletionTarget {
    Command { start: usize, prefix: String },
    Path { start: usize, parent: String, prefix: String },
    None,
}
fn completion_target(line: &str, pos: usize) -> CompletionTarget
```

Rules (consider only `line[..pos]`):
- If the cursor is still within the first whitespace-delimited token (no space
  before it) → `Command { start = token start byte offset, prefix = token }`.
- Else the cursor is in a later token (an argument). Let `tok` be the token
  containing the cursor:
  - If `tok` starts with `/` → `Path`:
    - `start` = byte offset of the character just after the LAST `/` in `tok`.
    - `prefix` = the segment after that last `/`.
    - `parent` = the text of `tok` up to (not including) that last `/`; if the
      last `/` is the leading one (`/arc`), `parent = "/"`.
    - Examples: `/arc` → parent `/`, prefix `arc`; `/arcus/` → parent
      `/arcus`, prefix ``; `/arcus/ca` → parent `/arcus`, prefix `ca`.
  - Otherwise (argument not starting with `/`) → `None`.

### `Completer::complete` behavior

- `Command { start, prefix }` → candidates = command names in
  `["ls","get","create","set","delete","stat","quit"]` starting with `prefix`;
  each `Pair { display: name, replacement: name }`. Return `(start, pairs)`.
- `Path { start, parent, prefix }` → call `get_children(parent, false)` on the
  shared `Rc<ZooKeeper>`; keep names starting with `prefix`; each
  `Pair { display: name, replacement: name }`. On a ZK error, return an empty
  candidate list (completion never errors the editor). Return `(start, pairs)`.
- `None` → return `(pos, vec![])`.

Only the final path segment is replaced (shell-style), because `start` points
just past the last `/`.

## 4. Testing

- **Pure, unit-tested:**
  - Extended `parse()` for `ls`: `-R`, `-s`, `-R -s`, flags before/after path,
    no path → Unknown, two paths → Unknown, unknown flag → Unknown, plain
    `ls /a` still `recursive=false, stat=false`.
  - `completion_target()`: first-token command case; path split at last `/`;
    root (`/arc`); trailing slash (`/arcus/`); mid-path (`/a/b/c`); non-`/`
    argument → None; empty line → Command with empty prefix (or None — see
    below).
- **Manual (live ZooKeeper):** `ls -R /`, `ls -s /arcus`, `ls -R -s /arcus`,
  and pressing Tab to complete a command (`cr`→`create`) and a path
  (`ls /arc`→`/arcus`, `get /arcus/`→ lists children).

Empty line completion: `completion_target("", 0)` returns
`Command { start: 0, prefix: "" }` so Tab on an empty line lists all commands.

## 5. README

Update the ZooKeeper section: note `ls [-R] [-s] <path>` and that Tab
completes command names and existing znode paths.
