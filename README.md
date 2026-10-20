# Arcus CLI

An **Arcus-Memcached** Command Line Interface (CLI) built with Rust.
It provides a seamless interactive experience for managing a standalone Arcus server with support for multiple network protocols and secure authentication.

## Features

- **Direct Server Connection**
- **Multi-Protocol(TCP, UDP, Unix Domain) Support**
- **SASL Authentication**
- **Advanced REPL**
- **Auto-Reconnection**
- **ZooKeeper Mode (zkCli-like)**

## Execution

You can run the project directly using cargo run. Remember to pass CLI arguments after the -- separator.

```bash
# Basic TCP connection
cargo run -- --host 127.0.0.1 --port 11211

# Enable SASL authentication
cargo run -- --host 127.0.0.1 --sasl

# Connect via UDP
cargo run -- --host 127.0.0.1 --udp

# Connect via Unix Domain Socket
cargo run -- --unix --host /tmp/arcus.sock

# Connect to a ZooKeeper ensemble (like zkCli); history is disabled
cargo run -- --zookeeper --host 127.0.0.1 --port 2181
```

## Memcached REPL

- Type a command and the remaining syntax is hinted in cyan, following the
  arcus-memcached command reference. Press `→` at the end of a hint to
  complete a partially typed command word.
- Commands that carry data (`set`, `lop insert`, `mop get`, `bop smget`, ...)
  continue on the next line until the data is complete: a `<bytes>` long
  value (it may span several lines, joined with `\r\n`) or one line of keys.
  `mop get|delete <key> 0 0` and `bop update ... -1` take no data line.
- Several commands can be pasted at once; each is sent as its own request.
- `Ctrl-C`, `Ctrl-D` or `quit` exits. While data lines are awaited,
  `Ctrl-C` only discards that input; press it again to exit.
- `--timeout <ms>` (default 1000) is how long to wait for UDP replies and
  for replies still in flight on exit.

In ZooKeeper mode the REPL supports: `ls [-R] [-s] <path>` (`-R` recursive,
`-s` with stat), `get <path>`, `create <path> [data]`, `set <path> <data>`,
`delete <path>`, `stat <path>`, and `quit`. Press Tab to complete command
names and existing znode paths at the current path.

## Installation

To install the arcus-cli binary globally to your system path, use cargo install. This allows you to run the tool from any directory without using cargo run.

```bash
# Install from the current directory
cargo install --path .
```

Once installed, you can simply run:

```bash
arcus-cli --host 127.0.0.1
```

## MCP mode

`arcus-cli --mcp` runs a stdio [MCP](https://modelcontextprotocol.io) server
so an AI client can operate ARCUS for you: talk to a cache server, browse
ZooKeeper, and build, configure and run ARCUS servers from source.
Long operations (build, download, login) run as background jobs that the
client polls with `arcus_job_status`.

### Registering the server

```bash
# Claude Code
claude mcp add arcus -- arcus-cli --mcp
```

Gemini CLI, `~/.gemini/settings.json`:

```json
{"mcpServers": {"arcus": {"command": "arcus-cli", "args": ["--mcp"]}}}
```

Codex, `~/.codex/config.toml`:

```toml
[mcp_servers.arcus]
command = "arcus-cli"
args = ["--mcp"]
```

### Tools

| Tool | Purpose |
|------|---------|
| `memcached_command` | Send ASCII commands to a cache server (TCP or Unix socket) |
| `zk_ls` / `zk_get` / `zk_create` / `zk_set` | Browse and edit ZooKeeper znodes |
| `zk_password` | Store a generated SCRAM-SHA-256 secret in ZooKeeper; the password goes to a private file, never into the conversation |
| `arcus_list_features` | Optional build features per edition (public / ee) |
| `arcus_check_prerequisites` | Report missing build tools and the command to install them |
| `arcus_list_versions` | Installable server versions per edition |
| `arcus_ee_login` | GitHub device-code login for the private EE sources |
| `arcus_install` | Build and install a server from source (background job) |
| `arcus_job_status` | Poll a background job |
| `arcus_zk_start` / `arcus_zk_stop` | Start or stop a local ZooKeeper ensemble of 1-7 nodes (downloads it first if needed). `count`, `client_ports`, `peer_ports`, `election_ports` (or `base_*_port`) and `version` are all chosen by the user; the recommended values are 3 nodes on 2181-2183 / 2888-2890 / 3888-3890. The running ensemble is recorded in `zk/ensemble.json`, and `arcus_setup` with `zk: "local"` uses it |
| `arcus_setup` | Define a server instance and create its znodes; plan only unless `apply=true` |
| `arcus_start` / `arcus_stop` / `arcus_status` | Run, stop and inspect instances (`arcus_stop` uses SIGTERM; `force` sends SIGKILL) |

### EE login

`arcus_ee_login` starts the GitHub device flow. It returns a one-time code and
a URL; open the URL, enter the code and approve. The token stays in gh's own
credential store; `arcus-cli` does not save it, and it never passes through the
AI conversation. Afterwards
`arcus_list_versions` and `arcus_install` work for `edition: "ee"`.

### Files

Everything lives under `~/.arcus-cli/` (override with `--home <dir>`):
downloaded sources and builds, the local ZooKeeper, job logs, and
`instances/<name>/` for each instance (configuration, logs and, with SASL,
the generated `sasl_password`).
