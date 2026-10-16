//! Parses command syntax strings such as
//! `bop insert <key> <bkey> [<eflag>] <bytes> [noreply|getrim]`
//! and matches typed input against them to produce the remaining hint.

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Literal(String),
    Choice(Vec<String>),
    Var { name: String, repeat: bool },
    Optional(Vec<Node>),
}

pub fn parse(syntax: &str) -> Vec<Node> {
    let mut stack: Vec<Vec<Node>> = vec![Vec::new()];

    for raw in syntax.split_whitespace() {
        let opens = raw.chars().take_while(|&c| c == '[').count();
        let mut core = &raw[opens..];
        let inner_opens = core.matches('[').count();
        let closes = core.matches(']').count().saturating_sub(inner_opens);
        core = &core[..core.len() - closes];

        for _ in 0..opens { stack.push(Vec::new()); }

        let node = if core == "..." {
            if let Some(Node::Var { repeat, .. }) = stack.last_mut().unwrap().last_mut() {
                *repeat = true;
            }
            None
        } else if core.starts_with('<') {
            Some(Node::Var { name: core.to_string(), repeat: false })
        } else if core.contains('|') {
            Some(Node::Choice(core.split('|').map(String::from).collect()))
        } else {
            Some(Node::Literal(core.to_string()))
        };
        stack.last_mut().unwrap().extend(node);

        for _ in 0..closes {
            if stack.len() > 1 {
                let group = stack.pop().unwrap();
                stack.last_mut().unwrap().push(Node::Optional(group));
            }
        }
    }
    while stack.len() > 1 {
        let group = stack.pop().unwrap();
        stack.last_mut().unwrap().push(Node::Optional(group));
    }
    stack.pop().unwrap()
}

pub fn render(nodes: &[&Node]) -> String {
    nodes.iter().map(|n| render_node(n)).collect::<Vec<_>>().join(" ")
}

fn render_node(node: &Node) -> String {
    match node {
        Node::Literal(w) => w.clone(),
        Node::Choice(c) => c.join("|"),
        Node::Var { name, repeat: false } => name.clone(),
        Node::Var { name, repeat: true } => format!("{} ...", name),
        Node::Optional(sub) => format!("[{}]", render(&sub.iter().collect::<Vec<_>>())),
    }
}

fn required_count(nodes: &[&Node]) -> usize {
    nodes.iter().filter(|n| !matches!(n, Node::Optional(_))).count()
}

/// Result of matching typed tokens against a syntax.
#[derive(Debug, Clone)]
pub struct Match<'a> {
    /// Higher is better: literal matches weigh far more than variables.
    pub score: i32,
    /// Completion of a partially typed literal (shown right after the cursor).
    pub suffix: String,
    /// Syntax elements still to be typed.
    pub rest: Vec<&'a Node>,
}

impl Match<'_> {
    fn better_than(&self, other: &Match) -> bool {
        (self.score, -(required_count(&self.rest) as i32))
            > (other.score, -(required_count(&other.rest) as i32))
    }
}

/// `tokens` are fully typed words; `partial` is the word under the cursor, if any.
pub fn match_syntax<'a>(nodes: &'a [Node], tokens: &[&str], partial: Option<&str>) -> Option<Match<'a>> {
    let items: Vec<&Node> = nodes.iter().collect();
    walk(&items, tokens, partial)
}

fn best<'a>(a: Option<Match<'a>>, b: Option<Match<'a>>) -> Option<Match<'a>> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if y.better_than(&x) { y } else { x }),
        (x, None) => x,
        (None, y) => y,
    }
}

fn add(m: Option<Match>, gain: i32) -> Option<Match> {
    m.map(|mut m| { m.score += gain; m })
}

fn walk<'a>(items: &[&'a Node], tokens: &[&str], partial: Option<&str>) -> Option<Match<'a>> {
    let Some((&first, rest)) = items.split_first() else {
        return if tokens.is_empty() && partial.is_none() {
            Some(Match { score: 0, suffix: String::new(), rest: Vec::new() })
        } else {
            None
        };
    };

    if tokens.is_empty() && partial.is_none() {
        return Some(Match { score: 0, suffix: String::new(), rest: items.to_vec() });
    }

    if let Node::Optional(sub) = first {
        let mut expanded: Vec<&Node> = sub.iter().collect();
        expanded.extend_from_slice(rest);
        return best(walk(rest, tokens, partial), walk(&expanded, tokens, partial));
    }

    if let Some((&tok, more)) = tokens.split_first() {
        return match first {
            Node::Literal(w) if w == tok => add(walk(rest, more, partial), 100),
            Node::Choice(c) if c.iter().any(|w| w == tok) => add(walk(rest, more, partial), 50),
            Node::Var { repeat, .. } => {
                let next = add(walk(rest, more, partial), 1);
                if *repeat { best(next, add(walk(items, more, partial), 1)) } else { next }
            }
            _ => None,
        };
    }

    match partial {
        None => unreachable!(),
        Some(p) => {
            let (gain, suffix) = match first {
                Node::Literal(w) if w.starts_with(p) => (5, w[p.len()..].to_string()),
                Node::Choice(c) => match c.iter().find(|w| w.starts_with(p)) {
                    Some(w) => (4, w[p.len()..].to_string()),
                    None => return None,
                },
                Node::Var { .. } => (1, String::new()),
                _ => return None,
            };
            Some(Match { score: gain, suffix, rest: rest.to_vec() })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hint(syntax: &str, line: &str) -> Option<String> {
        let nodes = parse(syntax);
        let words: Vec<&str> = line.split_whitespace().collect();
        let (tokens, partial) = if line.ends_with(' ') || words.is_empty() {
            (&words[..], None)
        } else {
            (&words[..words.len() - 1], Some(words[words.len() - 1]))
        };
        match_syntax(&nodes, tokens, partial).map(|m| {
            let rest = render(&m.rest);
            let mut s = m.suffix.clone();
            if !rest.is_empty() {
                if partial.is_some() { s.push(' '); }
                s.push_str(&rest);
            }
            s
        })
    }

    #[test]
    fn parses_nested_optionals_and_ranges() {
        let n = parse("bop get <key> <bkey>[..<bkey>] [[<offset>] <count>] [delete|drop]");
        assert_eq!(n.len(), 6);
        assert_eq!(n[3], Node::Var { name: "<bkey>[..<bkey>]".into(), repeat: false });
        assert_eq!(render(&n.iter().collect::<Vec<_>>()),
                   "bop get <key> <bkey>[..<bkey>] [[<offset>] <count>] [delete|drop]");
    }

    #[test]
    fn parses_repeat() {
        let n = parse("get <key> [<key> ...]");
        assert_eq!(n[2], Node::Optional(vec![Node::Var { name: "<key>".into(), repeat: true }]));
        assert_eq!(render(&n.iter().collect::<Vec<_>>()), "get <key> [<key> ...]");
    }

    #[test]
    fn completes_partial_literal() {
        assert_eq!(hint("lop create <key> <flags>", "lop cr").as_deref(), Some("eate <key> <flags>"));
        assert_eq!(hint("lop create <key> <flags>", "lop x"), None);
    }

    #[test]
    fn shows_remaining_after_space() {
        assert_eq!(hint("set <key> <flags> <expiretime> <bytes> [noreply]", "set k 0 ").as_deref(),
                   Some("<expiretime> <bytes> [noreply]"));
        assert_eq!(hint("set <key> <flags> <expiretime> <bytes> [noreply]", "set k 0 0 5 noreply ").as_deref(),
                   Some(""));
    }

    #[test]
    fn literal_beats_variable() {
        let s = "lop insert <key> <index> <bytes> [create <flags> <expiretime> <maxcount> [<overflowaction>] [unreadable]] [noreply]";
        assert_eq!(hint(s, "lop insert k 0 5 create ").as_deref(),
                   Some("<flags> <expiretime> <maxcount> [<overflowaction>] [unreadable] [noreply]"));
        assert_eq!(hint(s, "lop insert k 0 5 noreply ").as_deref(), Some(""));
    }

    #[test]
    fn too_many_tokens_fails() {
        assert_eq!(hint("delete <key> [noreply]", "delete a noreply x "), None);
    }

    #[test]
    fn repeat_accepts_many() {
        assert_eq!(hint("get <key> [<key> ...]", "get a ").as_deref(), Some("[<key> ...]"));
        assert_eq!(hint("get <key> [<key> ...]", "get a b c d ").as_deref(), Some(""));
    }

    #[test]
    fn prefers_fewest_required_on_tie() {
        let s = "bop get <key> <bkey>[..<bkey>] [<offset> [<bitwop> <bitwvalue>] <compop> <compvalue>] [[<offset>] <count>] [delete|drop]";
        assert_eq!(hint(s, "bop get k 0..10 10 ").as_deref(), Some("[delete|drop]"));
    }
}
