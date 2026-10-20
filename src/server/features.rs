use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Edition {
    Public,
    Ee,
}

/// Declaration order fixes configure-argument and build-key order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Feature {
    Zk,
    Sasl,
    Repl,
    Persistence,
    Sticky,
    Migration,
    Squall,
    Gust,
}

impl Feature {
    pub const ALL: [Feature; 8] = [
        Feature::Zk, Feature::Sasl, Feature::Repl, Feature::Persistence,
        Feature::Sticky, Feature::Migration, Feature::Squall, Feature::Gust,
    ];

    pub fn recommended(self) -> bool {
        matches!(self, Feature::Zk | Feature::Sasl | Feature::Repl)
    }

    pub fn ee_only(self) -> bool {
        matches!(self, Feature::Repl | Feature::Migration | Feature::Squall | Feature::Gust)
    }

    pub fn configure_flag(self, prefix: &Path) -> Vec<String> {
        let p = prefix.display();
        match self {
            Feature::Zk => vec!["--enable-zk-integration".into(), format!("--with-zookeeper={p}")],
            Feature::Sasl => vec!["--enable-sasl".into()],
            Feature::Repl => vec!["--enable-replication".into()],
            Feature::Persistence => vec!["--enable-persistence".into()],
            Feature::Sticky => vec!["--enable-sticky-item".into()],
            Feature::Migration => vec!["--enable-migration".into()],
            Feature::Squall => vec!["--enable-squall-engine".into()],
            Feature::Gust => vec!["--enable-gust-engine".into()],
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Feature::Zk => "zk",
            Feature::Sasl => "sasl",
            Feature::Repl => "repl",
            Feature::Persistence => "persistence",
            Feature::Sticky => "sticky",
            Feature::Migration => "migration",
            Feature::Squall => "squall",
            Feature::Gust => "gust",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Feature::Zk => "ZooKeeper integration (cluster coordination)",
            Feature::Sasl => "SASL authentication",
            Feature::Repl => "Replication (master/slave)",
            Feature::Persistence => "Persistence (snapshot and command log)",
            Feature::Sticky => "Sticky items",
            Feature::Migration => "Data migration",
            Feature::Squall => "Squall storage engine",
            Feature::Gust => "Gust storage engine",
        }
    }
}

pub fn validate(edition: Edition, features: &BTreeSet<Feature>) -> Result<(), String> {
    if edition == Edition::Public
        && let Some(f) = features.iter().find(|f| f.ee_only()) {
            return Err(format!("feature '{}' is available only in the EE edition", f.key()));
        }
    for f in [Feature::Sasl, Feature::Repl] {
        if features.contains(&f) && !features.contains(&Feature::Zk) {
            return Err(format!("feature '{}' requires 'zk'", f.key()));
        }
    }
    if features.contains(&Feature::Squall) && features.contains(&Feature::Gust) {
        return Err("features 'squall' and 'gust' cannot be combined".into());
    }
    Ok(())
}

pub fn configure_args(features: &BTreeSet<Feature>, prefix: &Path) -> Vec<String> {
    let p = prefix.display();
    let mut args = vec![format!("--prefix={p}"), format!("--with-libevent={p}")];
    for f in features {
        args.extend(f.configure_flag(prefix));
    }
    args
}

pub fn build_key(edition: Edition, version: &str, features: &BTreeSet<Feature>) -> String {
    let ed = match edition {
        Edition::Public => "public",
        Edition::Ee => "ee",
    };
    let feats = if features.is_empty() {
        "base".to_string()
    } else {
        features.iter().map(|f| f.key()).collect::<Vec<_>>().join("_")
    };
    format!("{ed}-{version}-{feats}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::Path;

    #[test]
    fn configure_args_recommended_ee() {
        let f = BTreeSet::from([Feature::Zk, Feature::Sasl, Feature::Repl]);
        assert_eq!(configure_args(&f, Path::new("/h/b")), ["--prefix=/h/b", "--with-libevent=/h/b",
            "--enable-zk-integration", "--with-zookeeper=/h/b", "--enable-sasl", "--enable-replication"]);
    }
    #[test]
    fn advanced_flags() {
        let p = Path::new("/h/b");
        assert_eq!(Feature::Persistence.configure_flag(p), ["--enable-persistence"]);
        assert_eq!(Feature::Sticky.configure_flag(p), ["--enable-sticky-item"]);
        assert_eq!(Feature::Migration.configure_flag(p), ["--enable-migration"]);
        assert_eq!(Feature::Squall.configure_flag(p), ["--enable-squall-engine"]);
        assert_eq!(Feature::Gust.configure_flag(p), ["--enable-gust-engine"]);
    }
    #[test]
    fn validate_rules() {
        let s = |v: &[Feature]| v.iter().copied().collect::<BTreeSet<_>>();
        assert!(validate(Edition::Public, &s(&[Feature::Zk, Feature::Repl])).is_err());
        assert!(validate(Edition::Ee, &s(&[Feature::Repl])).is_err());
        assert!(validate(Edition::Ee, &s(&[Feature::Sasl])).is_err());
        assert!(validate(Edition::Ee, &s(&[Feature::Squall, Feature::Gust])).is_err());
        assert!(validate(Edition::Ee, &s(&[Feature::Zk, Feature::Sasl, Feature::Repl])).is_ok());
        assert!(validate(Edition::Public, &s(&[Feature::Zk, Feature::Sasl])).is_ok());
    }
    #[test]
    fn build_key_stable() {
        assert_eq!(build_key(Edition::Ee, "0.12.1", &BTreeSet::from([Feature::Repl, Feature::Zk])), "ee-0.12.1-zk_repl");
        assert_eq!(build_key(Edition::Public, "1.0", &BTreeSet::new()), "public-1.0-base");
    }
}
