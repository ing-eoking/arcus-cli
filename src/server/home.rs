use std::io;
use std::path::PathBuf;

/// Validated instance name: `[A-Za-z0-9_-]{1,64}` (safe as a single path component).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceName(String);

impl InstanceName {
    pub fn parse(s: &str) -> Result<InstanceName, String> {
        let ok = !s.is_empty() && s.len() <= 64
            && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if ok { Ok(InstanceName(s.to_string())) }
        else { Err(format!("invalid instance name `{s}`: use 1-64 characters of [A-Za-z0-9_-]")) }
    }
    pub fn as_str(&self) -> &str { &self.0 }
}

/// Data directory layout (default `$HOME/.arcus-cli`).
#[derive(Debug, Clone)]
pub struct Home { pub root: PathBuf }

impl Home {
    pub fn new(root: Option<PathBuf>) -> io::Result<Home> {
        let root = match root {
            Some(r) => r,
            None => {
                let home = std::env::var_os("HOME").filter(|h| !h.is_empty())
                    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "$HOME is not set; pass --home <dir>"))?;
                PathBuf::from(home).join(".arcus-cli")
            }
        };
        std::fs::create_dir_all(&root)?;
        Ok(Home { root })
    }
    pub fn downloads(&self) -> PathBuf { self.root.join("downloads") }
    pub fn build_prefix(&self, key: &str) -> PathBuf { self.root.join("builds").join(key) }
    pub fn zookeeper_dist(&self, version: &str) -> PathBuf { self.root.join("zookeeper").join(version) }
    pub fn zk_dir(&self) -> PathBuf { self.root.join("zk") }
    pub fn jobs(&self) -> PathBuf { self.root.join("jobs") }
    pub fn gh_dir(&self) -> PathBuf { self.root.join("gh") }
    pub fn secrets(&self) -> PathBuf { self.root.join("secrets") }
    pub fn instance(&self, name: &InstanceName) -> PathBuf { self.root.join("instances").join(name.as_str()) }
}
