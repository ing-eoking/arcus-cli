mod grammar;
mod spec;

use std::borrow::Cow::{self, Owned};
use std::cell::Cell;
use rustyline::Context;
use rustyline::highlight::Highlighter;
use rustyline::hint::{Hint, Hinter};
use rustyline::validate::{ValidationContext, ValidationResult, Validator};
use rustyline_derive::{Completer, Helper};
use crate::request;
use self::grammar::{Node, match_syntax, render};

struct CommandSpec {
    nodes: Vec<Node>,
    /// Hint for the data line read after the command line, if any.
    data: Option<String>,
}

#[derive(Helper, Completer)]
pub struct MyHelper {
    specs: Vec<CommandSpec>,
    /// The input being edited already spans several lines, i.e. the
    /// command line was accepted and its data line(s) are awaited.
    continuing: Cell<bool>,
}

impl MyHelper {
    pub fn new() -> Self {
        let specs = spec::command_syntaxes().iter().map(|s| {
            let mut parts = s.splitn(2, '\n');
            CommandSpec {
                nodes: grammar::parse(parts.next().unwrap_or("")),
                data: parts.next().map(String::from),
            }
        }).collect();
        MyHelper { specs, continuing: Cell::new(false) }
    }

    /// Whether the last edited input was waiting for data lines. Clears it.
    pub fn take_continuing(&self) -> bool { self.continuing.replace(false) }

    /// Hint for a command line being typed.
    fn command_hint(&self, line: &str) -> Option<CMDHint> {
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.is_empty() { return None; }
        let (tokens, partial) = if line.ends_with(char::is_whitespace) {
            (&words[..], None)
        } else {
            (&words[..words.len() - 1], Some(words[words.len() - 1]))
        };

        let matches: Vec<_> = self.specs.iter()
            .filter_map(|s| match_syntax(&s.nodes, tokens, partial))
            .collect();
        let top = matches.iter().map(|m| m.score).max()?;
        let tied: Vec<_> = matches.iter().filter(|m| m.score == top).collect();

        /* several subcommands fit (e.g. "bop "): list them */
        if partial.is_none() && tied.len() > 1 {
            let mut words: Vec<&str> = Vec::new();
            let all_literal = tied.iter().all(|m| match m.rest.first() {
                Some(Node::Literal(w)) => { if !words.contains(&w.as_str()) { words.push(w); } true }
                _ => false,
            });
            if all_literal {
                return Some(CMDHint { display: words.join("|"), completion: None });
            }
        }

        let best = tied[0];
        let rest = render(&best.rest);
        let mut display = best.suffix.clone();
        if !rest.is_empty() {
            if partial.is_some() { display.push(' '); }
            display.push_str(&rest);
        }
        if display.is_empty() { return None; }
        let completion = (!best.suffix.is_empty()).then(|| best.suffix.clone());
        Some(CMDHint { display, completion })
    }

    /// Hint for the data line of `command`.
    fn data_hint(&self, command: &str) -> Option<CMDHint> {
        let words: Vec<&str> = command.split_whitespace().collect();
        self.specs.iter()
            .filter_map(|s| match_syntax(&s.nodes, &words, None).map(|m| (m.score, s)))
            .max_by_key(|(score, _)| *score)
            .and_then(|(_, s)| s.data.clone())
            .map(|display| CMDHint { display, completion: None })
    }
}

impl Highlighter for MyHelper {
    fn highlight_hint<'h>(&self, hint: &'h str) -> Cow<'h, str> {
        Owned("\x1b[96m".to_owned() + hint + "\x1b[m")
    }
}

impl Validator for MyHelper {
    fn validate(&self, ctx: &mut ValidationContext) -> rustyline::Result<ValidationResult> {
        if request::split(ctx.input()).pending.is_empty() {
            Ok(ValidationResult::Valid(None))
        } else {
            Ok(ValidationResult::Incomplete)
        }
    }
}

impl Hinter for MyHelper {
    type Hint = CMDHint;

    fn hint(&self, line: &str, pos: usize, _ctx: &Context<'_>) -> Option<Self::Hint> {
        self.continuing.set(line.contains('\n'));
        if pos < line.len() || line.is_empty() { return None; }

        let (before, current) = match line.rfind('\n') {
            Some(i) => (&line[..i], &line[i + 1..]),
            None => ("", line),
        };
        let split = request::split(before);
        match split.pending.as_slice() {
            [] => self.command_hint(current),
            [command] if current.is_empty() => self.data_hint(command),
            _ => None,
        }
    }
}

#[derive(Hash, Debug, PartialEq, Eq, Clone)]
pub struct CMDHint {
    display: String,
    completion: Option<String>,
}

impl Hint for CMDHint {
    fn display(&self) -> &str { &self.display }
    fn completion(&self) -> Option<&str> { self.completion.as_deref() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hint(line: &str) -> Option<String> {
        let h = MyHelper::new();
        let (before, current) = match line.rfind('\n') {
            Some(i) => (&line[..i], &line[i + 1..]),
            None => ("", line),
        };
        match request::split(before).pending.as_slice() {
            [] => h.command_hint(current),
            [command] if current.is_empty() => h.data_hint(command),
            _ => None,
        }.map(|h| h.display)
    }

    #[test]
    fn every_spec_parses() {
        for s in spec::command_syntaxes() {
            let first = s.split('\n').next().unwrap();
            let nodes = grammar::parse(first);
            assert_eq!(render(&nodes.iter().collect::<Vec<_>>()), first);
        }
    }

    #[test]
    fn command_hints() {
        assert_eq!(hint("se").as_deref(), Some("t <key> <flags> <expiretime> <bytes> [noreply]"));
        assert_eq!(hint("set").as_deref(), Some(" <key> <flags> <expiretime> <bytes> [noreply]"));
        assert_eq!(hint("cas k 0 0 5 ").as_deref(), Some("<cas_unique> [noreply]"));
        assert_eq!(hint("lop ").as_deref(), Some("create|insert|get|delete"));
        assert_eq!(hint("mop up").as_deref(), Some("sert <key> <mkey> <bytes> [create <flags> <expiretime> <maxcount> [<overflowaction>] [unreadable]] [noreply]"));
        assert_eq!(hint("stats ").as_deref(), Some("[settings|items|slabs|prefixes|zookeeper|scrub|dump|persistence|reset]"));
        assert_eq!(hint("stats detail ").as_deref(), Some("on|off|dump"));
        assert_eq!(hint("bop gbp k ").as_deref(), Some("asc|desc <position>[..<position>]"));
        assert_eq!(hint("incr k 1 ").as_deref(), Some("[<flags> <expiretime> <initial>] [noreply]"));
        assert_eq!(hint("delete k noreply"), None);
    }

    #[test]
    fn data_line_hints() {
        assert_eq!(hint("set k 0 0 5\n").as_deref(), Some("<value>"));
        assert_eq!(hint("mget 3 1\n").as_deref(), Some("<key> <key> ..."));
        assert_eq!(hint("mop get k 3 1\n").as_deref(), Some("<mkey> <mkey> ..."));
        assert_eq!(hint("set k 0 0 5\nhel"), None);
        assert_eq!(hint("set k 0 0 1\na\nge").as_deref(), Some("t <key> [<key> ...]"));
    }
}
