use std::borrow::Cow::{self, Owned};
use std::collections::HashSet;
use rustyline::Context;
use rustyline::highlight::Highlighter;
use rustyline::hint::{Hint,Hinter};
use rustyline_derive::{Completer, Helper, Highlighter, Validator, Hinter};

#[derive(Helper, Completer, Hinter, Validator)]
pub struct MyHelper {
    #[rustyline(Hinter)]
    hinter: CMDHinter,
}

impl MyHelper {
    pub fn new() -> Self {
        MyHelper { hinter: CMDHinter { hints: arcus_hints() } }
    }
}

impl Highlighter for MyHelper {
    fn highlight_hint<'h>(&self, hint: &'h str) -> Cow<'h, str> {
        Owned("\x1b[96m".to_owned() + hint + "\x1b[m")
    }
}

#[derive(Completer, Helper, Validator, Highlighter)]
pub struct CMDHinter { hints: HashSet<CMDHint> }

#[derive(Hash, Debug, PartialEq, Eq)]
pub struct CMDHint {
    display: String,
    complete_up_to: usize,
}

impl Hint for CMDHint {
    fn display(&self) -> &str { &self.display }

    fn completion(&self) -> Option<&str> {
        if self.complete_up_to > 0 {
            Some(&self.display[..self.complete_up_to])
        }
        else { None }
    }
}

impl Hinter for CMDHinter {
    type Hint = CMDHint;

    fn hint(&self, line: &str, pos: usize, _ctx: &Context<'_>) -> Option<CMDHint> {
        if line.is_empty() || pos < line.len() { return None; }
        self.hints.iter().filter_map(|hint| {
            if hint.display.starts_with(line) {
                Some(hint.suffix(pos))
            } else {
                None
            }
        }).next()
    }
}

impl CMDHint {
    fn new(text: &str, complete_up_to: &str) -> CMDHint {
        assert!(text.starts_with(complete_up_to));
        CMDHint {
            display: text.into(),
            complete_up_to: complete_up_to.len(),
        }
    }

    fn suffix(&self, strip_chars: usize) -> CMDHint {
        CMDHint {
            display: self.display[strip_chars..].to_owned(),
            complete_up_to: self.complete_up_to.saturating_sub(strip_chars),
        }
    }
}

pub fn arcus_hints() -> HashSet<CMDHint> {
    let mut set = HashSet::new();
    // K/V
    set.insert(CMDHint::new("get <key>", "get "));
    set.insert(CMDHint::new("gets <key>", "gets "));
    set.insert(CMDHint::new("mget <lenkeys> <numkeys>\n<\"space separated keys\">", "mget "));
    set.insert(CMDHint::new("mgets <lenkeys> <numkeys>\n<\"space separated keys\">", "mgets "));
    set.insert(CMDHint::new("set <key> <flags> <exptime> <bytes> [noreply]\n<data>", "set "));
    set.insert(CMDHint::new("cas <key> <flags> <exptime> <bytes> [noreply]\n<data>", "cas "));
    set.insert(CMDHint::new("add <key> <flags> <exptime> <bytes> [noreply]\n<data>", "add "));
    set.insert(CMDHint::new("append <key> <flags> <exptime> <bytes> [noreply]\n<data>", "append "));
    set.insert(CMDHint::new("prepend <key> <flags> <exptime> <bytes> [noreply]\n<data>", "prepend "));
    set.insert(CMDHint::new("replace <key> <flags> <exptime> <bytes> [noreply]\n<data>", "replace "));
    set.insert(CMDHint::new("delete <key> [noreply]", "delete "));
    set.insert(CMDHint::new("incr <key> <delta> [<flags> <exptime> <initial>] [noreply]", "incr "));
    set.insert(CMDHint::new("decr <key> <delta> [<flags> <exptime> <initial>] [noreply]", "decr "));
    // List
    set.insert(CMDHint::new("lop create <key> <attributes> [noreply]\n* attributes: <flags> <exptime> <maxcount> [<ovflaction>] [unreadable]", "lop create "));
    set.insert(CMDHint::new("lop insert <key> <index> <bytes> [create <attributes>] [noreply|pipe]\n* attributes: <flags> <exptime> <maxcount> [<ovflaction>] [unreadable]\n<data>", "lop insert "));
    set.insert(CMDHint::new("lop delete <key> <index or \"index range\"> [drop] [noreply|pipe]", "lop delete "));
    set.insert(CMDHint::new("lop get <key> <index or \"index range\"> [delete|drop]", "lop get "));
    // Set
    set.insert(CMDHint::new("sop create <key> <attributes> [noreply]\n* <attributes>: <flags> <exptime> <maxcount> [<ovflaction>] [unreadable]", "sop create "));
    set.insert(CMDHint::new("sop insert <key> <bytes> [create <attributes>] [noreply|pipe]\n* <attributes>: <flags> <exptime> <maxcount> [<ovflaction>] [unreadable]\n<data>", "sop insert "));
    set.insert(CMDHint::new("sop delete <key> <bytes> [drop] [noreply|pipe]\n<data>", "sop delete "));
    set.insert(CMDHint::new("sop get <key> <count> [delete|drop]", "sop get "));
    set.insert(CMDHint::new("sop exist <key> <bytes> [pipe]\n<data>", "sop exist "));
    // Map
    set.insert(CMDHint::new("mop create <key> <attributes> [noreply]\n* <attributes>: <flags> <exptime> <maxcount> [<ovflaction>] [unreadable]", "mop create "));
    set.insert(CMDHint::new("mop insert <key> <field> <bytes> [create <attributes>] [noreply|pipe]\n* <attributes>: <flags> <exptime> <maxcount> [<ovflaction>] [unreadable]\n<data>", "mop insert "));
    set.insert(CMDHint::new("mop update <key> <field> <bytes> [noreply|pipe]\n<data>", "mop update "));
    set.insert(CMDHint::new("mop delete <key> <lenfields> <numfields> [drop] [noreply|pipe]\n[<\"space separated fields\">]", "mop delete "));
    set.insert(CMDHint::new("mop get <key> <lenfields> <numfields> [delete|drop]\n[<\"space separated fields\">]\n", "mop get "));
    // Btree
    set.insert(CMDHint::new("bop create <key> <attributes> [noreply]", "bop create "));
    set.insert(CMDHint::new("bop insert <key> <bkey> [<eflag>] <bytes> [create <attributes>] [noreply|pipe|getrim]\n* attributes: <flags> <exptime> <maxcount> [<ovflaction>] [unreadable]\n<data>", "bop insert "));
    set.insert(CMDHint::new("bop upsert <key> <bkey> [<eflag>] <bytes> [create <attributes>] [noreply|pipe|getrim]\n* attributes: <flags> <exptime> <maxcount> [<ovflaction>] [unreadable]\n<data>", "bop upsert "));
    set.insert(CMDHint::new("bop update <key> <bkey> [<eflag_update>] <bytes> [noreply|pipe]\n* eflag_update : [<fwhere> <bitwop>] <fvalue>\n[<data>]", "bop update "));
    set.insert(CMDHint::new("bop delete <key> <bkey or \"bkey range\"> [<eflag_filter>] [<count>] [drop] [noreply|pipe]\n* <eflag_filter> : <fwhere> [<bitwop> <foperand>] <compop> <fvalue>", "bop delete "));
    set.insert(CMDHint::new("bop get <key> <bkey or \"bkey range\"> [<eflag_filter>] [[<offset>] <count>] [delete|drop]\n* <eflag_filter> : <fwhere> [<bitwop> <foperand>] <compop> <fvalue>", "bop get "));
    set.insert(CMDHint::new("bop count <key> <bkey or \"bkey range\"> [<eflag_filter>]\n* <eflag_filter> : <fwhere> [<bitwop> <foperand>] <compop> <fvalue>", "bop count "));
    set.insert(CMDHint::new("bop incr <key> <bkey> <delta> [<initial> [<eflag>]] [noreply|pipe]", "bop incr "));
    set.insert(CMDHint::new("bop decr <key> <bkey> <delta> [<initial> [<eflag>]] [noreply|pipe]", "bop decr "));
    set.insert(CMDHint::new("bop mget <lenkeys> <numkeys> <bkey or \"bkey range\"> [<eflag_filter>] [<offset>] <count>\n* <eflag_filter> : <fwhere> [<bitwop> <foperand>] <compop> <fvalue>\n<\"space separated keys\">", "bop mget "));
    set.insert(CMDHint::new("bop smget <lenkeys> <numkeys> <bkey or \"bkey range\"> [<eflag_filter>] <count> [duplicate|unique]\n* <eflag_filter> : <fwhere> [<bitwop> <foperand>] <compop> <fvalue>\n<\"space separated keys\">", "bop smget "));
    set.insert(CMDHint::new("bop position <key> <bkey> <order>\n* <order> = asc | desc", "bop position "));
    set.insert(CMDHint::new("bop gbp <key> <order> <position or \"position range\">\n", "bop gbp "));
    set.insert(CMDHint::new("bop pwg <key> <bkey> <order> [<count>]\n* <order> = asc | desc", "bop pwg "));
    set
}
