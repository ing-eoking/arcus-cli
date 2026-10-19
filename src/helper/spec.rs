//! Command syntax shown as hints, following the arcus-memcached command
//! reference (arcus-web-sites `docs/arcus-memcached@test/3-명령어`).
//! Compound parameters of the reference (`<attributes>`, `<new_data>`,
//! `<eflag_filter>`, `<range>`, ...) are expanded so that every word typed
//! lines up with one hint word. Commands the reference does not cover yet
//! follow the server `help` output of arcus-memcached develop.
//!
//! A second line, when present, is the hint for the data line that follows.

const ATTRS: &str = "<flags> <expiretime> <maxcount> [<overflowaction>] [unreadable]";
const EFLAG_FILTER: &str = "[<offset> [<bitwop> <bitwvalue>] <compop> <compvalue>]";

pub fn command_syntaxes() -> Vec<String> {
    let raw: &[&str] = &[
        // Key-Value
        "set <key> <flags> <expiretime> <bytes> [noreply]\n<value>",
        "add <key> <flags> <expiretime> <bytes> [noreply]\n<value>",
        "replace <key> <flags> <expiretime> <bytes> [noreply]\n<value>",
        "prepend <key> <flags> <expiretime> <bytes> [noreply]\n<value>",
        "append <key> <flags> <expiretime> <bytes> [noreply]\n<value>",
        "cas <key> <flags> <expiretime> <bytes> <cas_unique> [noreply]\n<value>",
        "touch <key> <expiretime> [noreply]",
        "incr <key> <delta> [<flags> <expiretime> <initial>] [noreply]",
        "decr <key> <delta> [<flags> <expiretime> <initial>] [noreply]",
        "get <key> [<key> ...]",
        "gets <key> [<key> ...]",
        "mget <lenkeys> <numkeys>\n<key> <key> ...",
        "mgets <lenkeys> <numkeys>\n<key> <key> ...",
        "gat <expiretime> <key> [<key> ...]",
        "gats <expiretime> <key> [<key> ...]",
        "delete <key> [noreply]",
        // List
        "lop create <key> {ATTRS} [noreply]",
        "lop insert <key> <index> <bytes> [create {ATTRS}] [noreply]\n<value>",
        "lop get <key> <index>[..<index>] [delete|drop]",
        "lop delete <key> <index>[..<index>] [drop] [noreply]",
        // Set
        "sop create <key> {ATTRS} [noreply]",
        "sop insert <key> <bytes> [create {ATTRS}] [noreply]\n<value>",
        "sop get <key> <count> [delete|drop]",
        "sop exist <key> <bytes>\n<value>",
        "sop delete <key> <bytes> [drop] [noreply]\n<value>",
        // Map
        "mop create <key> {ATTRS} [noreply]",
        "mop insert <key> <mkey> <bytes> [create {ATTRS}] [noreply]\n<value>",
        "mop upsert <key> <mkey> <bytes> [create {ATTRS}] [noreply]\n<value>",
        "mop update <key> <mkey> <bytes> [noreply]\n<value>",
        "mop get <key> <lenmkeys> <nummkeys> [delete|drop]\n<mkey> <mkey> ...",
        "mop delete <key> <lenmkeys> <nummkeys> [drop] [noreply]\n<mkey> <mkey> ...",
        // B+Tree
        "bop create <key> {ATTRS} [noreply]",
        "bop insert <key> <bkey> [<eflag>] <bytes> [create {ATTRS}] [noreply|getrim]\n<value>",
        "bop upsert <key> <bkey> [<eflag>] <bytes> [create {ATTRS}] [noreply|getrim]\n<value>",
        "bop update <key> <bkey> [[<offset> <bitwop>] <eflag>] <bytes> [noreply]\n<value>",
        "bop incr <key> <bkey> <delta> [<initial> [<eflag>]] [noreply]",
        "bop decr <key> <bkey> <delta> [<initial> [<eflag>]] [noreply]",
        "bop get <key> <bkey>[..<bkey>] {EFLAG_FILTER} [[<offset>] <count>] [delete|drop]",
        "bop count <key> <bkey>[..<bkey>] {EFLAG_FILTER}",
        "bop mget <lenkeys> <numkeys> <bkey>[..<bkey>] {EFLAG_FILTER} [<offset>] <count>\n<key> <key> ...",
        "bop smget <lenkeys> <numkeys> <bkey>[..<bkey>] {EFLAG_FILTER} <count> duplicate|unique\n<key> <key> ...",
        "bop position <key> <bkey> asc|desc",
        "bop gbp <key> asc|desc <position>[..<position>]",
        "bop pwg <key> <bkey> asc|desc [<count>]",
        "bop delete <key> <bkey>[..<bkey>] {EFLAG_FILTER} [<count>] [drop] [noreply]",
        // Item attributes
        "setattr <key> <attribute>=<value> [<attribute>=<value> ...]",
        "getattr <key> [<attribute> ...]",
        // Statistics
        "stats [settings|items|slabs|prefixes|zookeeper|scrub|dump|persistence|reset]",
        "stats detail on|off|dump",
        "stats cachedump <slab_clsid> <limit> [forward|backward [sticky]]",
        // Dynamic configuration
        "config verbosity [<value>]",
        "config memlimit [<value>]",
        "config maxconns [<value>]",
        "config max_list_size [<value>]",
        "config max_set_size [<value>]",
        "config max_map_size [<value>]",
        "config max_btree_size [<value>]",
        "config max_element_bytes [<value>]",
        "config scrub_count [<value>]",
        "config max_stats_prefixes [<value>]",
        "config zkfailstop [on|off]",
        "config hbtimeout [<value>]",
        "config hbfailstop [<value>]",
        "config auth [on]",
        "config sticky_limit [<value>]",
        "config chkpt_interval_pct_snapshot [<value>]",
        "config chkpt_interval_min_logsize [<value>]",
        "config async_logging [on|off]",
        // Data management
        "flush_all [<delay>] [noreply]",
        "flush_prefix <prefix> [<delay>] [noreply]",
        "scrub [stale]",
        "scan key <cursor> [count <count>] [match <pattern>] [type <type>]",
        "scan prefix <cursor> [count <count>] [match <pattern>]",
        "dump start key [<prefix>] <filepath>",
        "dump stop",
        // Cluster
        "zkensemble set <ensemble_list>",
        "zkensemble get",
        "zkensemble rejoin",
        // Security
        "sasl mech",
        "sasl auth [<mechanism>] <bytes>\n<data>",
        "reload auth",
        // Logging
        "cmdlog start [<file_path>]",
        "cmdlog stop",
        "cmdlog stats",
        "lqdetect start [<detect_threshold>]",
        "lqdetect stop",
        "lqdetect show",
        "lqdetect stats",
        // Misc
        "help [kv|list|set|map|btree|attr|scan|admin]",
        "version",
        "ready",
        "shutdown [<seconds>]",
        "quit",
    ];
    raw.iter()
        .map(|s| s.replace("{ATTRS}", ATTRS).replace("{EFLAG_FILTER}", EFLAG_FILTER))
        .collect()
}
