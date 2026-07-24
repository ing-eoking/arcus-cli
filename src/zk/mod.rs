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
