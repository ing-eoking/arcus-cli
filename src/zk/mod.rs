use std::time::Duration;
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;
use zookeeper::{ZooKeeper, Acl, CreateMode, WatchedEvent};

#[derive(Debug)]
pub enum ZkCommand {
    Ls { path: String, recursive: bool, stat: bool },
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
}

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

fn join_zk_path(parent: &str, child: &str) -> String {
    if parent == "/" {
        format!("/{}", child)
    } else {
        format!("{}/{}", parent, child)
    }
}

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

    /// Runs one command. Returns true when the loop should exit.
    fn execute(&self, cmd: ZkCommand) -> bool {
        match cmd {
            ZkCommand::Quit => return true,
            ZkCommand::Empty => {}
            ZkCommand::Unknown(msg) => eprintln!("ERROR: {}", msg),
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
                Ok(Some(stat)) => self.print_stat(&stat),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_line_is_empty() {
        assert!(matches!(parse("   "), ZkCommand::Empty));
        assert!(matches!(parse(""), ZkCommand::Empty));
    }

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

    #[test]
    fn join_zk_path_root_and_nested() {
        assert_eq!(join_zk_path("/", "arcus"), "/arcus");
        assert_eq!(join_zk_path("/arcus", "cache_list"), "/arcus/cache_list");
        assert_eq!(join_zk_path("/a/b", "c"), "/a/b/c");
    }
}
