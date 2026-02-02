use std::borrow::Cow::{self, Owned};
use std::collections::{HashMap, HashSet};
use rustyline::Context;
use rustyline::highlight::Highlighter;
use rustyline::hint::{Hint, Hinter};
use rustyline::validate::{ValidationContext, ValidationResult, Validator};
use rustyline_derive::{Completer, Helper};

#[derive(Helper, Completer)]
pub struct MyHelper {
    pub hints_map: HashMap<String, Vec<CMDHint>>,
    pub keys: Vec<String>,
}

impl MyHelper {
    pub fn new() -> Self {
        let all_hints = arcus_hints();
        let mut map: HashMap<String, Vec<CMDHint>> = HashMap::new();
        let mut keys = Vec::new();

        for hint in all_hints {
            let first_word = hint.command_prefix.split_whitespace().next().unwrap_or("").to_string();
            map.entry(first_word.clone()).or_insert_with(Vec::new).push(hint);
            if !keys.contains(&first_word) { keys.push(first_word); }
        }

        MyHelper { hints_map: map, keys }
    }

    fn find_best_hint(&self, line: &str) -> Option<(CMDHint, String)> {
        let line = line.trim_end();
        if line.is_empty() { return None; }

        let input_tokens: Vec<&str> = line.split_whitespace().collect();
        let first_word = input_tokens[0];

        if let Some(group) = self.hints_map.get(first_word) {
            let group_refs: Vec<&CMDHint> = group.iter().collect();
            return self.find_best_in_group(&group_refs, line, &input_tokens);
        }

        let mut candidates = Vec::new();
        for key in &self.keys {
            if key.starts_with(first_word) {
                if let Some(group) = self.hints_map.get(key) {
                    candidates.extend(group);
                }
            }
        }

        if !candidates.is_empty() {
            return self.find_best_in_group(&candidates, line, &input_tokens);
        }

        None
    }

    fn find_best_in_group<'a>(&self, group: &[&'a CMDHint], line: &str, input_tokens: &[&str]) -> Option<(CMDHint, String)> {
        let mut best_match: Option<CMDHint> = None;
        let mut best_score = -1;
        let mut best_display = String::new();

        for hint_ref in group {
            let hint = *hint_ref;
            if !line.starts_with(&hint.command_prefix) && !hint.command_prefix.starts_with(line) {
                continue;
            }

            let hint_first_line = hint.display.split('\n').next().unwrap_or("");
            let hint_tokens: Vec<&str> = hint_first_line.split_whitespace().collect();

            let mut future_literals = HashSet::new();
            for t in &hint_tokens {
                let clean = t.trim_matches(|c| c == '[' || c == ']' || c == '<' || c == '>');
                if !t.contains('<') && !clean.contains('|') {
                    future_literals.insert(clean);
                }
            }

            let (score, parts) = recursive_match(&hint_tokens, input_tokens, 0, 0, &future_literals);

            if score > best_score {
                best_score = score;
                best_match = Some(hint.clone());
                best_display = parts.join(" ");
            } else if score == best_score {
                 if let Some(prev) = &best_match {
                     if hint.display.len() > prev.display.len() {
                         best_match = Some(hint.clone());
                         best_display = parts.join(" ");
                     }
                 } else {
                     best_match = Some(hint.clone());
                     best_display = parts.join(" ");
                 }
            }
        }

        if let Some(h) = best_match {
            return Some((h, best_display));
        }
        None
    }
}

fn recursive_match(
    hint_tokens: &[&str],
    input_tokens: &[&str],
    h_idx: usize,
    i_idx: usize,
    future_literals: &HashSet<&str>
) -> (i32, Vec<String>) {
    if i_idx >= input_tokens.len() {
        let mut parts = Vec::new();
        for i in h_idx..hint_tokens.len() {
            parts.push(hint_tokens[i].to_string());
        }
        return (0, parts);
    }

    if h_idx >= hint_tokens.len() {
        return (-9999, Vec::new());
    }

    let h_word = hint_tokens[h_idx];
    let i_word = input_tokens[i_idx];

    let clean_h = h_word.trim_matches(|c| c == '[' || c == ']' || c == '<' || c == '>');
    let is_variable = h_word.contains('<');
    let is_choice = clean_h.contains('|');
    let is_optional_block = h_word.starts_with('[');

    let is_current_editing_token = i_idx == input_tokens.len() - 1;

    let mut score_consume = -9999;
    let mut parts_consume = Vec::new();

    let mut matched = false;
    let mut current_gain = 0;
    let mut show_str = String::new();

    let is_future_keyword = future_literals.contains(i_word);
    let guard_blocked = is_variable && is_future_keyword;

    if !guard_blocked {
        if !is_variable && !is_choice && clean_h == i_word {
            matched = true; current_gain = 1000;
            show_str = "".to_string();
        } else if is_variable {
            matched = true; current_gain = 10;
            show_str = "".to_string();
        } else if is_choice {
            let choices: Vec<&str> = clean_h.split('|').collect();
            if let Some(f) = choices.iter().find(|c| c.starts_with(i_word)) {
                matched = true; current_gain = 20;
                let suffix = &f[i_word.len()..];
                show_str = format!("{}", suffix);
            }
        } else if clean_h.starts_with(i_word) {
            matched = true; current_gain = 5;
            let suffix = &clean_h[i_word.len()..];
            show_str = format!("{}", suffix);
        }
    }

    if matched {
        let (next_score, mut next_parts) = recursive_match(hint_tokens, input_tokens, h_idx + 1, i_idx + 1, future_literals);
        if next_score > -5000 {
            score_consume = current_gain + next_score;

            if is_current_editing_token {
                parts_consume.push(show_str);
            } else if !show_str.is_empty() {
                parts_consume.push(show_str);
            }

            parts_consume.append(&mut next_parts);
        }
    }

    let mut score_skip = -9999;
    let mut parts_skip = Vec::new();

    if is_optional_block {
        if let Some(next_h_idx) = find_skip_target(hint_tokens, h_idx) {
            let (next_score, mut next_parts) = recursive_match(hint_tokens, input_tokens, next_h_idx, i_idx, future_literals);
            if next_score > -5000 {
                score_skip = next_score;
                parts_skip.append(&mut next_parts);
            }
        }
    }

    if score_consume >= score_skip && score_consume > -5000 {
        return (score_consume, parts_consume);
    } else if score_skip > score_consume && score_skip > -5000 {
        return (score_skip, parts_skip);
    } else {
        return (-9999, Vec::new());
    }
}

fn find_skip_target(tokens: &[&str], start_idx: usize) -> Option<usize> {
    let mut depth = 0;
    for i in start_idx..tokens.len() {
        depth += tokens[i].matches('[').count();
        depth = depth.saturating_sub(tokens[i].matches(']').count());
        if depth == 0 {
            return Some(i + 1);
        }
    }
    None
}

fn count_required_args(hint_line: &str) -> usize {
    let mut count = 0;
    let mut depth = 0;
    for token in hint_line.split_whitespace() {
        let open = token.matches('[').count();
        let close = token.matches(']').count();
        if depth == 0 && open == 0 { count += 1; }
        depth += open;
        depth = depth.saturating_sub(close);
    }
    count
}

impl Highlighter for MyHelper {
    fn highlight_hint<'h>(&self, hint: &'h str) -> Cow<'h, str> {
        Owned("\x1b[96m".to_owned() + hint + "\x1b[m")
    }
}

impl Validator for MyHelper {
    fn validate(&self, ctx: &mut ValidationContext) -> rustyline::Result<ValidationResult> {
        let input = ctx.input();
        if input.contains('\n') { return Ok(ValidationResult::Valid(None)); }

        if let Some((hint, _)) = self.find_best_hint(input) {
            let hint_first_line = hint.display.split('\n').next().unwrap_or("");
            let required_parts = count_required_args(hint_first_line);
            let input_parts = input.split_whitespace().count();

            if input_parts < required_parts {
                return Ok(ValidationResult::Incomplete);
            }
            if hint.display.contains('\n') {
                return Ok(ValidationResult::Incomplete);
            }
        }
        Ok(ValidationResult::Valid(None))
    }
}

impl Hinter for MyHelper {
    type Hint = CMDHint;

    fn hint(&self, line: &str, pos: usize, _ctx: &Context<'_>) -> Option<Self::Hint> {
        if pos < line.len() || line.is_empty() { return None; }

        let all_lines: Vec<&str> = line.split('\n').collect();
        let current_line_idx = all_lines.len() - 1;

        if current_line_idx >= 1 {
            if let Some((hint, _)) = self.find_best_hint(line) {
                 let hint_parts: Vec<&str> = hint.display.split('\n').collect();
                 if hint_parts.len() > 1 && all_lines[current_line_idx].is_empty() {
                     return Some(CMDHint {
                        display: format!(" {}", hint_parts[1]),
                        command_prefix: hint.command_prefix.clone(),
                    });
                 }
            }
            return None;
        }

        let is_trailing_space = line.ends_with(' ');

        if let Some((hint, display_str)) = self.find_best_hint(line) {
            let mut final_hint = display_str;
            if is_trailing_space {
                final_hint = final_hint.trim_start().to_string();
            }

            return Some(CMDHint {
                display: final_hint,
                command_prefix: hint.command_prefix
            });
        }
        None
    }
}

#[derive(Hash, Debug, PartialEq, Eq, Clone)]
pub struct CMDHint {
    pub display: String,
    pub command_prefix: String,
}
impl Hint for CMDHint {
    fn display(&self) -> &str { &self.display }
    fn completion(&self) -> Option<&str> { None }
}

pub fn arcus_hints() -> Vec<CMDHint> {
    let mut hints = vec![
        // K/V
        ("get <key>", "get"),
        ("gets <key>", "gets"),
        ("gat <exptime> <key>", "gat"),
        ("gats <exptime> <key>", "gats"),
        ("mget <lenkeys> <numkeys>\n<space_separated_keys>", "mget"),
        ("mgets <lenkeys> <numkeys>\n<space_separated_keys>", "mgets"),
        ("set <key> <flags> <exptime> <bytes> [noreply]\n<data>", "set"),
        ("cas <key> <flags> <exptime> <bytes> [noreply]\n<data>", "cas"),
        ("add <key> <flags> <exptime> <bytes> [noreply]\n<data>", "add"),
        ("append <key> <flags> <exptime> <bytes> [noreply]\n<data>", "append"),
        ("prepend <key> <flags> <exptime> <bytes> [noreply]\n<data>", "prepend"),
        ("replace <key> <flags> <exptime> <bytes> [noreply]\n<data>", "replace"),
        ("delete <key> [noreply]", "delete"),
        ("incr <key> <delta> [<flags> <exptime> <initial>] [noreply]", "incr"),
        ("decr <key> <delta> [<flags> <exptime> <initial>] [noreply]", "decr"),
        ("touch <key> <exptime> [noreply]", "touch"),
        // List
        ("lop create <key> <flags> <exptime> <maxcount> [<ovflaction>] [unreadable] [noreply]", "lop create"),
        ("lop insert <key> <index> <bytes> [create <flags> [<exptime> <maxcount> [<ovflaction>] [unreadable]]] [noreply|pipe]\n<data>", "lop insert"),
        ("lop delete <key> <index or \"index range\"> [drop] [noreply|pipe]", "lop delete "),
        ("lop get <key> <index or \"index range\"> [delete|drop]", "lop get "),
        // Set
        ("sop create <key> <flags> <exptime> <maxcount> [<ovflaction>] [unreadable] [noreply]", "sop create"),
        ("sop insert <key> <bytes> [create <flags> <exptime> <maxcount> [<ovflaction>] [unreadable]] [noreply|pipe]\n<data>", "sop insert"),
        ("sop get <key> <count> [delete|drop]", "sop get"),
        ("sop exist <key> <bytes> [pipe]\n<data>", "sop exist"),
        // Map
        ("mop create <key> <flags> <exptime> <maxcount> [<ovflaction>] [unreadable] [noreply]", "mop create"),
        ("mop insert <key> <field> <bytes> [create <flags> <exptime> <maxcount> [<ovflaction>] [unreadable]] [noreply|pipe]\n<data>", "mop insert"),
        ("mop update <key> <field> <bytes> [noreply|pipe]\n<data>", "mop update"),
        ("mop delete <key> <lenfields> <numfields> [drop] [noreply|pipe]\n[<space_separated_fields>]", "mop delete"),
        ("mop get <key> <lenfields> <numfields> [delete|drop]\n<space_separated_fields>\n", "mop get"),
        ("mop get <key> 0 0 [delete|drop]", "mop get"),
        // Btree
        ("bop create <key> <flags> <exptime> <maxcount> [<ovflaction>] [unreadable] [noreply]", "bop create"),
        ("bop insert <key> <bkey> [<eflag>] <bytes> [create <flags> <exptime> <maxcount> [<ovflaction>] [unreadable]] [noreply|pipe|getrim]\n<data>", "bop insert"),
        ("bop upsert <key> <bkey> [<eflag>] <bytes> [create <flags> <exptime> <maxcount> [<ovflaction>] [unreadable]] [noreply|pipe|getrim]\n<data>", "bop upsert"),
        ("bop update <key> <bkey> [[<fwhere> <bitwop>] <fvalue>] <bytes> [noreply|pipe]\n<data>", "bop update"),
        ("bop update <key> <bkey> [[<fwhere> <bitwop>] <fvalue>] -1 [noreply|pipe]", "bop update"),
        ("bop delete <key> <bkey|bkey_range> [<fwhere> [<bitwop> <foperand>] <compop> <fvalue>] [<count>] [drop] [noreply|pipe]", "bop delete"),
        ("bop get <key> <bkey|bkey_range> [<fwhere> [<bitwop> <foperand>] <compop> <fvalue>] [[<offset>] <count>] [delete|drop]", "bop get"),
        ("bop count <key> <bkey|bkey_range> [<fwhere> [<bitwop> <foperand>] <compop> <fvalue>]", "bop count"),
        ("bop incr <key> <bkey> <delta> [<initial> [<eflag>]] [noreply|pipe]", "bop incr"),
        ("bop decr <key> <bkey> <delta> [<initial> [<eflag>]] [noreply|pipe]", "bop decr"),
        ("bop mget <lenkeys> <numkeys> <bkey|bkey_range> [<fwhere> [<bitwop> <foperand>] <compop> <fvalue>] [<offset>] <count>\n<space_separated_keys>", "bop mget"),
        ("bop smget <lenkeys> <numkeys> <bkey|bkey range> [<fwhere> [<bitwop> <foperand>] <compop> <fvalue>] <count> [duplicate|unique]\n<space_separated_keys>", "bop smget"),
        ("bop position <key> <bkey> <asc|desc>", "bop position"),
        ("bop gbp <key> <order> <position|position_range>\n", "bop gbp"),
        ("bop pwg <key> <bkey> <asc|desc> [<count>]", "bop pwg"),
        // Item attributes
        ("getattr <key> [<name>]", "getattr"),
        ("setattr <key> <name>=<value>", "setattr"),
        // Scan
        ("scan key <cursor> [count <count>] [match <pattern>] [type <type>]", "scan key"),
        ("scan prefix <cursor> [count <count>] [match <pattern>]", "scan prefix"),
        // Admin
        ("flush_all [<delay>] [noreply]", "flush_all"),
        ("flush_prefix <prefix> [<delay>] [noreply]", "flush_prefix"),
        ("scrub [stale]", "scrub"),
        ("stats [settings|items|slabs|prefix|zookeeper]", "stats"),
        ("stats cachedump <slab_clsid> <limit> [forward|backward [sticky]]", "stats cachedump"),
        ("stats dump", "stats dump"),
        ("config verbosity [<verbose>]", "config verbosity"),
        ("config memlimit [<memsize>]", "config memlimit"),
        ("config zkfailstop [on|off]", "config zkfailstop"),
        ("config hbtimeout [<hbtimeout>]", "config hbtimeout"),
        ("config hbfailstop [hbfailstop]", "config hbfailstop"),
        ("config maxconns [<maxconn>]", "config maxconns"),
        ("config max_list_size [<max_size>]", "config max_list_size"),
        ("config max_set_size [<max_size>]", "config max_set_size"),
        ("config max_btree_size [<max_size>]", "config max_btree_size"),
        ("config max_map_size [<max_size>]", "config max_map_size"),
        ("config max_element_bytes [<maxbytes>]", "config max_element_bytes"),
        ("config scrub_count [<scrub_count>]", "config scrub_count"),
        ("cmdlog [start [<log_file_path>] | stop | stats]", "cmdlog"),
        ("dump start key [<prefix>] <filepath>", "dump start key"),
        ("dump stop", "dump stop"),
        ("zkensemble set <ensemble_list>", "zkensemble set"),
        ("zkensemble get", "zkensemble get"),
        ("zkensemble rejoin", "zkensemble rejoin"),
        ("help [<subcommand>]", "help"),
    ];
    hints.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    hints.into_iter().map(|(d, p)| CMDHint { display: d.into(), command_prefix: p.into() }).collect()
}
