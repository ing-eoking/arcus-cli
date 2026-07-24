use std::rc::Rc;
use std::time::Duration;
use rustyline::{Editor, Context};
use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::history::DefaultHistory;
use rustyline_derive::{Helper, Hinter, Highlighter, Validator};
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

fn join_zk_path(parent: &str, child: &str) -> String {
    if parent == "/" {
        format!("/{}", child)
    } else {
        format!("{}/{}", parent, child)
    }
}

const CMDS: [&str; 7] = ["ls", "get", "create", "set", "delete", "stat", "quit"];

struct ZkClient {
    zk: Rc<ZooKeeper>,
}

impl ZkClient {
    fn new(zk: Rc<ZooKeeper>) -> ZkClient {
        ZkClient { zk }
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
}
