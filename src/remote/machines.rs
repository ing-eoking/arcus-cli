//! Remote machine resolution: `~/.ssh/config` aliases and the `hosts.toml` registry.

use std::io;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use crate::server::home::Home;

const MAX_INCLUDE_DEPTH: usize = 8;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Target {
    SshAlias,
    Explicit { host: String, user: String, port: u16, identity_file: Option<String> },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Machine {
    pub name: String,
    pub target: Target,
}

impl Machine {
    /// Arguments identifying the destination for `ssh`.
    pub fn ssh_destination(&self) -> Vec<String> {
        match &self.target {
            Target::SshAlias => vec![self.name.clone()],
            Target::Explicit { host, user, port, identity_file } => {
                let mut a = vec!["-p".to_string(), port.to_string(), "-l".to_string(), user.clone()];
                if let Some(f) = identity_file { a.push("-i".to_string()); a.push(f.clone()); }
                a.push(host.clone());
                a
            }
        }
    }
}

/// Split a config line into (lowercased keyword, rest). Supports `Key value` and `Key=value`.
fn split_keyword(line: &str) -> Option<(String, &str)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') { return None; }
    let end = line.find(|c: char| c.is_whitespace() || c == '=')?;
    let key = line[..end].to_ascii_lowercase();
    let rest = line[end..].trim_start().trim_start_matches('=').trim();
    Some((key, rest))
}

fn glob_match(pat: &[u8], s: &[u8]) -> bool {
    match pat.split_first() {
        None => s.is_empty(),
        Some((b'*', rest)) => (0..=s.len()).any(|i| glob_match(rest, &s[i..])),
        Some((b'?', rest)) => !s.is_empty() && glob_match(rest, &s[1..]),
        Some((c, rest)) => s.first() == Some(c) && glob_match(rest, &s[1..]),
    }
}

/// Expand an `Include` pattern (wildcards in the final component only), sorted.
fn expand_include(pattern: &str, base: &Path) -> Vec<PathBuf> {
    let pattern = pattern.trim_matches('"');
    let p = if let Some(rest) = pattern.strip_prefix("~/") {
        match std::env::var_os("HOME") { Some(h) => PathBuf::from(h).join(rest), None => return vec![] }
    } else if Path::new(pattern).is_absolute() {
        PathBuf::from(pattern)
    } else {
        base.join(pattern)
    };
    let Some(file) = p.file_name().and_then(|f| f.to_str()).map(str::to_string) else { return vec![] };
    if !file.contains(['*', '?']) { return vec![p]; }
    let dir = p.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    let mut out: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd.filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_str().is_some_and(|n| glob_match(file.as_bytes(), n.as_bytes())))
            .map(|e| e.path()).collect(),
        Err(_) => vec![],
    };
    out.sort();
    out
}

fn collect_aliases(path: &Path, base: &Path, depth: usize, out: &mut Vec<String>) -> io::Result<()> {
    let text = std::fs::read_to_string(path)?;
    for line in text.lines() {
        let Some((key, rest)) = split_keyword(line) else { continue };
        match key.as_str() {
            "host" => for name in rest.split_whitespace() {
                let name = name.trim_matches('"');
                if name.is_empty() || name.contains(['*', '?', '!']) { continue; }
                if !out.iter().any(|n| n == name) { out.push(name.to_string()); }
            },
            "include" if depth < MAX_INCLUDE_DEPTH => for pat in rest.split_whitespace() {
                for f in expand_include(pat, base) {
                    // Unreadable or missing included files are ignored, like ssh does.
                    let _ = collect_aliases(&f, base, depth + 1, out);
                }
            },
            _ => {}
        }
    }
    Ok(())
}

/// Non-wildcard `Host` aliases of an ssh config, following `Include` (relative to the
/// config's directory). Parses only; never runs `ssh`.
pub fn ssh_config_aliases(path: &Path) -> io::Result<Vec<String>> {
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    let mut out = Vec::new();
    collect_aliases(path, base, 0, &mut out)?;
    Ok(out)
}

#[derive(Serialize, Deserialize, Default)]
struct HostsFile {
    #[serde(default)]
    machine: Vec<Machine>,
}

pub fn load_machines(home: &Home) -> io::Result<Vec<Machine>> {
    match std::fs::read_to_string(home.hosts_file()) {
        Ok(t) => toml::from_str::<HostsFile>(&t)
            .map(|h| h.machine)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("hosts.toml: {e}"))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(e),
    }
}

pub fn save_machine(home: &Home, m: &Machine) -> io::Result<()> {
    let bad = |e: String| io::Error::new(io::ErrorKind::InvalidInput, e);
    if !valid_machine_name(&m.name) { return Err(bad(machine_name_error(&m.name))); }
    if let Target::Explicit { host, user, identity_file, .. } = &m.target {
        let opt = [("host", Some(host)), ("user", Some(user)), ("identity_file", identity_file.as_ref())];
        for (field, v) in opt {
            if v.is_some_and(|v| v.starts_with('-')) {
                return Err(bad(format!("invalid {field}: must not start with '-'")));
            }
        }
    }
    let mut all = load_machines(home)?;
    match all.iter_mut().find(|x| x.name == m.name) {
        Some(x) => *x = m.clone(),
        None => all.push(m.clone()),
    }
    let text = toml::to_string(&HostsFile { machine: all })
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    let path = home.hosts_file();
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)
}

/// Machine names: 1..=255 chars, no whitespace/control chars, no '/', not starting with '-'.
pub fn valid_machine_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 255 && !s.starts_with('-')
        && !s.chars().any(|c| c.is_whitespace() || c.is_control() || c == '/')
}

fn machine_name_error(s: &str) -> String {
    format!("invalid machine name `{s}`: use 1-255 characters, no whitespace, control characters or '/', not starting with '-'")
}

/// Registered machine first, then an ssh config alias.
pub fn resolve(home: &Home, name: &str, ssh_config: &Path) -> Result<Machine, String> {
    if !valid_machine_name(name) { return Err(machine_name_error(name)); }
    let registered = load_machines(home).map_err(|e| e.to_string())?;
    if let Some(m) = registered.into_iter().find(|m| m.name == name) { return Ok(m); }
    if ssh_config_aliases(ssh_config).unwrap_or_default().iter().any(|a| a == name) {
        return Ok(Machine { name: name.to_string(), target: Target::SshAlias });
    }
    Err(format!(
        "unknown machine `{name}`: not in ~/.ssh/config or hosts.toml. Ask the user for host, user, port (recommended 22) and optional identity_file, then call arcus_host_add"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("arcus-machines-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn fixture(d: &Path) -> PathBuf {
        std::fs::create_dir_all(d.join("conf.d")).unwrap();
        let main = d.join("config");
        std::fs::write(&main, "Host dev-1\n  HostName 10.0.0.1\nHost *\n  User x\nInclude conf.d/*\n").unwrap();
        std::fs::write(d.join("conf.d/a"), "host dev-2 dev-3\nHost dev-1 bad? !neg\n").unwrap();
        main
    }

    #[test]
    fn aliases_skip_wildcards_and_follow_include() {
        let d = tmp("alias");
        let main = fixture(&d);
        assert_eq!(ssh_config_aliases(&main).unwrap(), ["dev-1", "dev-2", "dev-3"]);
    }

    #[test]
    fn resolve_prefers_registered_then_alias() {
        let d = tmp("resolve");
        let main = fixture(&d);
        let home = Home::new(Some(d.join("home"))).unwrap();
        let explicit = Machine { name: "dev-9".into(), target: Target::Explicit {
            host: "h".into(), user: "u".into(), port: 22, identity_file: None } };
        save_machine(&home, &explicit).unwrap();
        assert_eq!(resolve(&home, "dev-1", &main).unwrap().target, Target::SshAlias);
        assert_eq!(resolve(&home, "dev-9", &main).unwrap(), explicit);
        assert!(resolve(&home, "nope", &main).unwrap_err().contains("arcus_host_add"));
    }

    #[test]
    fn save_upserts_and_roundtrips() {
        let d = tmp("save");
        let home = Home::new(Some(d)).unwrap();
        assert!(load_machines(&home).unwrap().is_empty());
        let mk = |port| Machine { name: "m".into(), target: Target::Explicit {
            host: "h".into(), user: "u".into(), port, identity_file: Some("/k".into()) } };
        save_machine(&home, &Machine { name: "a".into(), target: Target::SshAlias }).unwrap();
        save_machine(&home, &mk(22)).unwrap();
        save_machine(&home, &mk(2222)).unwrap();
        let all = load_machines(&home).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[1], mk(2222));
        assert!(save_machine(&home, &Machine { name: "a/b".into(), target: Target::SshAlias }).is_err());
    }

    #[test]
    fn dotted_alias_resolves_and_bad_names_rejected() {
        let d = tmp("dots");
        let main = d.join("config");
        std::fs::write(&main, "Host prod.example.com\n").unwrap();
        let home = Home::new(Some(d.join("home"))).unwrap();
        assert_eq!(resolve(&home, "prod.example.com", &main).unwrap().target, Target::SshAlias);
        for n in ["-oProxyCommand=x", "a b", "a/b", ""] {
            assert!(resolve(&home, n, &main).is_err(), "{n:?}");
            assert!(!valid_machine_name(n));
        }
    }

    #[test]
    fn save_rejects_option_like_values() {
        let home = Home::new(Some(tmp("optinj"))).unwrap();
        let mk = |h: &str, u: &str, i: Option<&str>| Machine { name: "m".into(), target: Target::Explicit {
            host: h.into(), user: u.into(), port: 22, identity_file: i.map(String::from) } };
        assert!(save_machine(&home, &mk("-oX", "u", None)).is_err());
        assert!(save_machine(&home, &mk("h", "-oX", None)).is_err());
        assert!(save_machine(&home, &mk("h", "u", Some("-oX"))).is_err());
        assert!(save_machine(&home, &mk("h", "u", Some("/k"))).is_ok());
    }

    #[test]
    fn explicit_destination_args() {
        let m = Machine { name: "x".into(), target: Target::Explicit {
            host: "h".into(), user: "u".into(), port: 2222, identity_file: Some("/k".into()) } };
        assert_eq!(m.ssh_destination(), ["-p", "2222", "-l", "u", "-i", "/k", "h"]);
        let a = Machine { name: "dev".into(), target: Target::SshAlias };
        assert_eq!(a.ssh_destination(), ["dev"]);
    }
}
