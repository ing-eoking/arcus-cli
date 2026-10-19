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