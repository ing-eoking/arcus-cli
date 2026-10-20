//! Znode provisioning for an ARCUS cache server instance.
use super::features::Feature;
use super::instance::InstanceConfig;
use serde::Serialize;
use std::io;
use std::net::IpAddr;

#[derive(Debug, PartialEq, Serialize)]
pub struct ZnodeSpec { pub path: String, pub value: Vec<u8> }

pub trait ZnodeStore {
    fn get(&self, p: &str) -> io::Result<Option<Vec<u8>>>;
    fn create(&self, p: &str, v: &[u8]) -> io::Result<()>;
    fn set(&self, p: &str, v: &[u8]) -> io::Result<()>;
    fn children(&self, p: &str) -> io::Result<Vec<String>>;
}

/// `ZnodeStore` over a live ZooKeeper session: persistent znodes with the open ACL (like the server itself).
pub struct ZkStore(zookeeper::ZooKeeper);

impl ZkStore {
    pub fn connect(ensemble: &str) -> io::Result<ZkStore> {
        use std::time::Duration;
        zookeeper::ZooKeeper::connect(ensemble, Duration::from_secs(5), |_ev: zookeeper::WatchedEvent| {})
            .map(ZkStore)
            .map_err(|e| io::Error::other(format!("Failed to connect to ZooKeeper at {ensemble}: {e:?}")))
    }
}

fn zk_err(path: &str, e: zookeeper::ZkError) -> io::Error {
    use zookeeper::ZkError::*;
    let kind = match e { NoNode => io::ErrorKind::NotFound, NodeExists => io::ErrorKind::AlreadyExists, _ => io::ErrorKind::Other };
    io::Error::new(kind, format!("{path}: {e:?}"))
}

impl ZnodeStore for ZkStore {
    fn get(&self, p: &str) -> io::Result<Option<Vec<u8>>> {
        match self.0.get_data(p, false) {
            Ok((v, _)) => Ok(Some(v)),
            Err(zookeeper::ZkError::NoNode) => Ok(None),
            Err(e) => Err(zk_err(p, e)),
        }
    }
    fn create(&self, p: &str, v: &[u8]) -> io::Result<()> {
        self.0.create(p, v.to_vec(), zookeeper::Acl::open_unsafe().clone(), zookeeper::CreateMode::Persistent)
            .map(|_| ()).map_err(|e| zk_err(p, e))
    }
    fn set(&self, p: &str, v: &[u8]) -> io::Result<()> {
        self.0.set_data(p, v.to_vec(), None).map(|_| ()).map_err(|e| zk_err(p, e))
    }
    fn children(&self, p: &str) -> io::Result<Vec<String>> {
        self.0.get_children(p, false).map_err(|e| zk_err(p, e))
    }
}

#[derive(Debug, PartialEq)]
pub enum Action { Create, Same, Differs { current: Vec<u8> } }

/// Permission value for a local development user.
/// Confirmed from EE `sasl_defs.c` `arcus_sasl_authz()` (the value of `/arcus_acl/<group>/<user>` is split on ','
/// into tokens kv, list, set, map, btree, scan, flush, attr, admin, logall) and docs/administration/sasl.md.
/// `logall` (audit-log every command) is deliberately left out.
const FULL_ACCESS: &str = "kv,list,set,map,btree,scan,flush,attr,admin";

fn spec(path: impl Into<String>, value: &[u8]) -> ZnodeSpec { ZnodeSpec { path: path.into(), value: value.to_vec() } }

/// The znodes one cache server needs, parents first.
///
/// Confirmed from EE `arcus_zk.c` (and the public server's `arcus_zk.c`):
/// - The server creates only its own ephemeral node `cache_list/<svc>/<ip:port>-<hostname>` (`arcus_create_ephemeral_znode`)
///   and date nodes under `cache_server_log` (`arcus_zk_log`); every `zoo_create` there is non-recursive, so the
///   parents below must pre-exist. `arcus_check_server_mapping` requires exactly one child under
///   `cache_server_mapping/<ip:port>` (replication: `<svc>^<group>^<listen ip:port>`).
/// - Migration: `arcus_build_cloud_stat_znodes` creates `cloud_stat/<svc>/{INTERNAL,INTERNAL/alter_pick,
///   INTERNAL/exist_pick,INTERNAL/migrations,alter_list,STATE^<type>^BEGIN}` itself, but only when a cluster
///   alter starts, and it never creates `cloud_stat/<svc>`; `sm_reload_cloud_stat_znodes` watches that directory
///   from startup and retries forever when it is missing. So only `cloud_stat` and `cloud_stat/<svc>` are planned.
/// - SASL: the server reads `/arcus_acl_mapping/<svc>` (value = ACL group) and `/arcus_acl/<group>/...`
///   (EE `sasl_auxprop.c`: user znode value = permissions, child `authPassword` = SCRAM secret).
pub fn plan(cfg: &InstanceConfig, ip: IpAddr, password_secret: Option<&str>) -> Vec<ZnodeSpec> {
    let (Some(zk), Some(svc)) = (&cfg.zk, &cfg.service_code) else { return Vec::new() };
    let r = if cfg.repl.is_some() { "/arcus_repl" } else { "/arcus" };
    let ipport = format!("{ip}:{}", cfg.port);
    let child = match &cfg.repl {
        Some(rc) => format!("{svc}^{}^{}", rc.group, rc.listen),
        None => svc.clone(),
    };
    let mut p = vec![
        spec(r, b""),
        spec(format!("{r}/cache_list"), b""),
        spec(format!("{r}/cache_list/{svc}"), b""),
        spec(format!("{r}/cache_server_mapping"), b""),
        spec(format!("{r}/cache_server_mapping/{ipport}"), b""),
        spec(format!("{r}/cache_server_mapping/{ipport}/{child}"), b""),
        spec(format!("{r}/cache_server_log"), b""),
    ];
    if let Some(rc) = &cfg.repl {
        p.push(spec(format!("{r}/group_list"), b""));
        p.push(spec(format!("{r}/group_list/{svc}"), b""));
        p.push(spec(format!("{r}/group_list/{svc}/{}", rc.group), b""));
    }
    if cfg.features.contains(&Feature::Migration) {
        p.push(spec(format!("{r}/cloud_stat"), b""));
        p.push(spec(format!("{r}/cloud_stat/{svc}"), b""));
    }
    if let Some(sasl) = &cfg.sasl {
        p.push(spec("/arcus_acl_mapping", b""));
        p.push(spec(format!("/arcus_acl_mapping/{svc}"), sasl.acl_group.as_bytes()));
        if let (true, Some(user)) = (zk.local, &sasl.user) {
            let g = format!("/arcus_acl/{}", sasl.acl_group);
            p.push(spec("/arcus_acl", b""));
            p.push(spec(g.clone(), b""));
            p.push(spec(format!("{g}/{user}"), FULL_ACCESS.as_bytes()));
            p.push(spec(format!("{g}/{user}/authPassword"), password_secret.unwrap_or("").as_bytes()));
        }
    }
    p
}

/// Compares the plan with the store. A planned empty value (a parent or marker node) only needs to exist.
pub fn diff(store: &dyn ZnodeStore, plan: Vec<ZnodeSpec>) -> io::Result<Vec<(ZnodeSpec, Action)>> {
    plan.into_iter().map(|z| {
        let action = match store.get(&z.path)? {
            None => Action::Create,
            Some(_) if z.value.is_empty() => Action::Same,
            Some(cur) if cur == z.value => Action::Same,
            Some(cur) => Action::Differs { current: cur },
        };
        Ok((z, action))
    }).collect()
}

/// Creates missing znodes and overwrites differing ones only when listed in `overwrite`. Returns the written paths.
pub fn apply(store: &dyn ZnodeStore, diffs: &[(ZnodeSpec, Action)], overwrite: &[String]) -> io::Result<Vec<String>> {
    let mut written = Vec::new();
    for (z, a) in diffs {
        match a {
            Action::Create => { store.create(&z.path, &z.value)?; written.push(z.path.clone()); }
            Action::Differs { .. } if overwrite.contains(&z.path) => { store.set(&z.path, &z.value)?; written.push(z.path.clone()); }
            _ => {}
        }
    }
    Ok(written)
}

/// Checks the conditions the server verifies at startup; returns human-readable problems.
pub fn verify(store: &dyn ZnodeStore, cfg: &InstanceConfig, ip: IpAddr) -> Vec<String> {
    let (Some(_), Some(svc)) = (&cfg.zk, &cfg.service_code) else { return Vec::new() };
    let r = if cfg.repl.is_some() { "/arcus_repl" } else { "/arcus" };
    let mut problems = Vec::new();
    let mapping = format!("{r}/cache_server_mapping/{ip}:{}", cfg.port);
    let n = store.children(&mapping).map(|c| c.len()).unwrap_or(0);
    if n != 1 { problems.push(format!("mapping {mapping} has {n} children (server needs exactly 1)")); }
    if let Some(sasl) = &cfg.sasl {
        let m = format!("/arcus_acl_mapping/{svc}");
        if store.get(&m).ok().flatten().is_none_or(|v| v.is_empty()) { problems.push(format!("{m} is empty")); }
        let g = format!("/arcus_acl/{}", sasl.acl_group);
        if store.get(&g).ok().flatten().is_none() { problems.push(format!("ACL group {g} does not exist")); }
    }
    problems
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::server::features::{Edition, Feature};
    use crate::server::instance::*;
    use std::cell::RefCell;
    use std::collections::{BTreeMap, BTreeSet};

    #[derive(Default)]
    pub struct MemStore(pub RefCell<BTreeMap<String, Vec<u8>>>);
    impl ZnodeStore for MemStore {
        fn get(&self, p: &str) -> io::Result<Option<Vec<u8>>> { Ok(self.0.borrow().get(p).cloned()) }
        fn create(&self, p: &str, v: &[u8]) -> io::Result<()> {
            let parent = &p[..p.rfind('/').unwrap()];
            let mut m = self.0.borrow_mut();
            if !parent.is_empty() && !m.contains_key(parent) { return Err(io::ErrorKind::NotFound.into()); }
            if m.contains_key(p) { return Err(io::ErrorKind::AlreadyExists.into()); }
            m.insert(p.to_string(), v.to_vec());
            Ok(())
        }
        fn set(&self, p: &str, v: &[u8]) -> io::Result<()> {
            match self.0.borrow_mut().get_mut(p) { Some(x) => { *x = v.to_vec(); Ok(()) } None => Err(io::ErrorKind::NotFound.into()) }
        }
        fn children(&self, p: &str) -> io::Result<Vec<String>> {
            let pre = format!("{p}/");
            Ok(self.0.borrow().keys().filter_map(|k| k.strip_prefix(&pre))
                .filter(|r| !r.contains('/')).map(str::to_string).collect())
        }
    }

    fn base() -> InstanceConfig {
        InstanceConfig {
            name: "dev".into(), edition: Edition::Ee, version: "0.12.1".into(),
            features: BTreeSet::from([Feature::Zk]), build_key: "k".into(), port: 11211, memory_mb: 64,
            zk: Some(ZkTarget { ensemble: "10.0.0.1:2181".into(), local: false }),
            service_code: Some("svc1".into()), sasl: None, repl: None,
        }
    }
    fn ip() -> IpAddr { "10.0.0.1".parse().unwrap() }
    fn has(p: &[ZnodeSpec], path: &str) -> bool { p.iter().any(|s| s.path == path) }

    #[test]
    fn plan_plain() {
        let p = plan(&base(), ip(), None);
        assert!(has(&p, "/arcus/cache_list/svc1"));
        assert!(has(&p, "/arcus/cache_server_mapping/10.0.0.1:11211/svc1"));
        assert!(has(&p, "/arcus/cache_server_log"));
        assert!(!has(&p, "/arcus/cloud_stat"));
        let pos = |s: &str| p.iter().position(|z| z.path == s).unwrap();
        assert!(pos("/arcus") < pos("/arcus/cache_list") && pos("/arcus/cache_list") < pos("/arcus/cache_list/svc1"));
    }

    #[test]
    fn plan_repl() {
        let mut c = base();
        c.features.insert(Feature::Repl);
        c.repl = Some(ReplConfig { group: "g1".into(), listen: "10.0.0.1:20211".into() });
        let p = plan(&c, ip(), None);
        assert!(has(&p, "/arcus_repl/cache_server_mapping/10.0.0.1:11211/svc1^g1^10.0.0.1:20211"));
        assert!(has(&p, "/arcus_repl/group_list/svc1/g1"));
        assert!(!has(&p, "/arcus/cache_list"));
    }

    #[test]
    fn plan_migration_adds_cloud_stat_parents() {
        let mut c = base();
        c.features.insert(Feature::Migration);
        let p = plan(&c, ip(), None);
        assert!(has(&p, "/arcus/cloud_stat") && has(&p, "/arcus/cloud_stat/svc1"));
        assert!(!p.iter().any(|s| s.path.contains("INTERNAL") || s.path.contains("STATE")));
    }

    #[test]
    fn plan_sasl_existing_only_mapping() {
        let mut c = base();
        c.sasl = Some(SaslConfig { acl_group: "grp".into(), user: None });
        let p = plan(&c, ip(), None);
        assert!(p.contains(&ZnodeSpec { path: "/arcus_acl_mapping/svc1".into(), value: b"grp".to_vec() }));
        assert!(!p.iter().any(|s| s.path.starts_with("/arcus_acl/")));
    }

    #[test]
    fn plan_sasl_local_user() {
        let mut c = base();
        c.zk.as_mut().unwrap().local = true;
        c.sasl = Some(SaslConfig { acl_group: "grp".into(), user: Some("dev".into()) });
        let p = plan(&c, ip(), Some("SECRET"));
        assert!(p.contains(&ZnodeSpec { path: "/arcus_acl/grp/dev".into(),
            value: b"kv,list,set,map,btree,scan,flush,attr,admin".to_vec() }));
        assert!(p.contains(&ZnodeSpec { path: "/arcus_acl/grp/dev/authPassword".into(), value: b"SECRET".to_vec() }));
    }

    #[test]
    fn differs_kept_unless_listed() {
        let mut c = base();
        c.sasl = Some(SaslConfig { acl_group: "grp".into(), user: None });
        let s = MemStore::default();
        for (p, v) in [("/arcus_acl_mapping", ""), ("/arcus_acl_mapping/svc1", "old")] {
            s.create(p, v.as_bytes()).unwrap();
        }
        let d = diff(&s, plan(&c, ip(), None)).unwrap();
        let w = apply(&s, &d, &[]).unwrap();
        assert!(!w.contains(&"/arcus_acl_mapping/svc1".to_string()));
        assert_eq!(s.get("/arcus_acl_mapping/svc1").unwrap().unwrap(), b"old");
        let d = diff(&s, plan(&c, ip(), None)).unwrap();
        assert!(d.iter().any(|(z, a)| z.path == "/arcus_acl_mapping/svc1" && matches!(a, Action::Differs { .. })));
        let w = apply(&s, &d, &["/arcus_acl_mapping/svc1".to_string()]).unwrap();
        assert!(w.contains(&"/arcus_acl_mapping/svc1".to_string()));
        assert_eq!(s.get("/arcus_acl_mapping/svc1").unwrap().unwrap(), b"grp");
    }

    #[test]
    fn empty_parent_is_same_and_apply_is_idempotent() {
        let s = MemStore::default();
        let c = base();
        let d = diff(&s, plan(&c, ip(), None)).unwrap();
        assert!(d.iter().all(|(_, a)| *a == Action::Create));
        assert!(!apply(&s, &d, &[]).unwrap().is_empty());
        let d = diff(&s, plan(&c, ip(), None)).unwrap();
        assert!(d.iter().all(|(_, a)| *a == Action::Same));
        assert!(apply(&s, &d, &[]).unwrap().is_empty());
    }

    #[test]
    fn verify_problems() {
        let mut c = base();
        c.sasl = Some(SaslConfig { acl_group: "grp".into(), user: None });
        let s = MemStore::default();
        for (p, v) in [("/arcus_acl", ""), ("/arcus_acl/grp", ""), ("/arcus_acl_mapping", ""), ("/arcus_acl_mapping/svc1", ""),
            ("/arcus", ""), ("/arcus/cache_server_mapping", ""), ("/arcus/cache_server_mapping/10.0.0.1:11211", ""),
            ("/arcus/cache_server_mapping/10.0.0.1:11211/svc1", ""), ("/arcus/cache_server_mapping/10.0.0.1:11211/svc2", "")] {
            s.create(p, v.as_bytes()).unwrap();
        }
        let v = verify(&s, &c, ip());
        assert_eq!(v.len(), 2, "{v:?}");
        assert!(v[0].contains("has 2 children") && v[1].contains("/arcus_acl_mapping/svc1 is empty"));
        s.0.borrow_mut().remove("/arcus_acl/grp");
        assert!(verify(&s, &c, ip()).iter().any(|p| p.contains("ACL group /arcus_acl/grp does not exist")));
    }
}
