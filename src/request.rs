//! Splits typed or pasted text into complete ASCII protocol requests.
//!
//! A request is a command line, followed by the lines the server reads
//! after it: a `<bytes>` long value (storage commands) or one line of
//! space separated keys/fields (mget, mop get/delete, bop mget/smget).

/// What the server reads after a command line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Body {
    None,
    /// `<bytes>` of data; may span several typed lines joined with `\r\n`.
    Value(usize),
    /// A single line, e.g. space separated keys.
    Line,
}

pub fn body_of(command: &str) -> Body {
    let t: Vec<&str> = command.split_whitespace().collect();
    let at = |i: usize| t.get(i).copied();
    let value = |i: usize| match at(i).and_then(|s| s.parse::<usize>().ok()) {
        Some(n) => Body::Value(n),
        None => Body::None,
    };

    match (at(0).unwrap_or(""), at(1).unwrap_or("")) {
        ("set" | "add" | "replace" | "append" | "prepend" | "cas", _) => value(4),
        ("mget" | "mgets", _) if t.len() >= 3 => Body::Line,
        ("lop", "insert") => value(4),
        ("sop", "insert" | "delete" | "exist") => value(3),
        ("mop", "insert" | "upsert" | "update") => value(4),
        ("mop", "get" | "delete") if t.len() >= 5 && at(4) != Some("0") => Body::Line,
        ("bop", "insert" | "upsert") => {
            /* the server treats a "0x" word after <bkey> as <eflag> */
            if t.len() > 5 && at(4).is_some_and(|s| s.starts_with("0x")) { value(5) } else { value(4) }
        }
        ("bop", "update") if t.len() >= 5 => {
            let mut last = t.len() - 1;
            if t[last] == "noreply" { last -= 1; }
            if last >= 4 { value(last) } else { Body::None }
        }
        ("bop", "mget" | "smget") if t.len() >= 6 => Body::Line,
        ("sasl", "auth") if t.len() >= 3 => value(t.len() - 1),
        _ => Body::None,
    }
}

#[derive(Debug, PartialEq)]
pub struct Request {
    pub lines: Vec<String>,
}

impl Request {
    pub fn command(&self) -> &str { &self.lines[0] }

    pub fn is_noreply(&self) -> bool {
        self.command().split_whitespace().any(|w| w == "noreply")
    }

    pub fn to_wire(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for line in &self.lines {
            out.extend_from_slice(line.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        out
    }
}

#[derive(Debug, PartialEq)]
pub struct Split {
    pub requests: Vec<Request>,
    /// Lines of a trailing request still waiting for its data.
    pub pending: Vec<String>,
}

pub fn split(input: &str) -> Split {
    let normalized = input.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    let mut requests = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let command = lines[i];
        if command.trim().is_empty() { i += 1; continue; }

        let start = i;
        i += 1;
        let complete = match body_of(command) {
            Body::None => true,
            Body::Line => {
                if i < lines.len() { i += 1; true } else { false }
            }
            Body::Value(bytes) => {
                let mut len: Option<usize> = None;
                while i < lines.len() && len.is_none_or(|l| l < bytes) {
                    len = Some(len.map_or(0, |l| l + 2) + lines[i].len());
                    i += 1;
                }
                len.is_some_and(|l| l >= bytes)
            }
        };

        let req_lines: Vec<String> = lines[start..i].iter().map(|s| s.to_string()).collect();
        if !complete {
            return Split { requests, pending: req_lines };
        }
        requests.push(Request { lines: req_lines });
    }
    Split { requests, pending: Vec::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(r: &Request) -> Vec<&str> { r.lines.iter().map(|s| s.as_str()).collect() }

    #[test]
    fn body_rules() {
        assert_eq!(body_of("set k 0 0 5"), Body::Value(5));
        assert_eq!(body_of("set k 0 0"), Body::None);
        assert_eq!(body_of("cas k 0 0 5 123"), Body::Value(5));
        assert_eq!(body_of("get k"), Body::None);
        assert_eq!(body_of("mget 3 1"), Body::Line);
        assert_eq!(body_of("lop insert k 0 3 create 0 0 10"), Body::Value(3));
        assert_eq!(body_of("sop exist k 3"), Body::Value(3));
        assert_eq!(body_of("mop upsert k f 3"), Body::Value(3));
        assert_eq!(body_of("mop get k 0 0"), Body::None);
        assert_eq!(body_of("mop get k 5 2"), Body::Line);
        assert_eq!(body_of("mop delete k 0 0 drop"), Body::None);
        assert_eq!(body_of("bop insert k 1 0x01 3"), Body::Value(3));
        assert_eq!(body_of("bop insert k 1 3"), Body::Value(3));
        assert_eq!(body_of("bop insert k 1 3 create 0 0 100 noreply"), Body::Value(3));
        assert_eq!(body_of("bop update k 1 0x01 -1"), Body::None);
        assert_eq!(body_of("bop update k 1 1 & 0x01 4 noreply"), Body::Value(4));
        assert_eq!(body_of("bop update k 1 4"), Body::Value(4));
        assert_eq!(body_of("bop gbp k asc 0..3"), Body::None);
        assert_eq!(body_of("bop smget 3 2 0..10 5 unique"), Body::Line);
        assert_eq!(body_of("sasl auth PLAIN 10"), Body::Value(10));
    }

    #[test]
    fn splits_pasted_commands() {
        let s = split("set a 0 0 1\r\na\r\nget a\r\n\r\nmget 3 2\r\na b\r\n");
        assert!(s.pending.is_empty());
        assert_eq!(s.requests.len(), 3);
        assert_eq!(lines(&s.requests[0]), ["set a 0 0 1", "a"]);
        assert_eq!(lines(&s.requests[1]), ["get a"]);
        assert_eq!(lines(&s.requests[2]), ["mget 3 2", "a b"]);
        assert_eq!(s.requests[0].to_wire(), b"set a 0 0 1\r\na\r\n");
    }

    #[test]
    fn waits_for_data() {
        let s = split("get a\nset k 0 0 5");
        assert_eq!(s.requests.len(), 1);
        assert_eq!(s.pending, ["set k 0 0 5"]);
    }

    #[test]
    fn value_spanning_lines() {
        let s = split("set k 0 0 7\nab\ncd");
        assert_eq!(s.pending, ["set k 0 0 7", "ab", "cd"]);
        let s = split("set k 0 0 7\nab\ncde\nget k");
        assert_eq!(lines(&s.requests[0]), ["set k 0 0 7", "ab", "cde"]);
        assert_eq!(s.requests[0].to_wire(), b"set k 0 0 7\r\nab\r\ncde\r\n");
        assert_eq!(lines(&s.requests[1]), ["get k"]);
    }

    #[test]
    fn empty_value() {
        let s = split("set k 0 0 0\n");
        assert_eq!(lines(&s.requests[0]), ["set k 0 0 0", ""]);
        assert_eq!(split("set k 0 0 0").pending, ["set k 0 0 0"]);
    }
}
