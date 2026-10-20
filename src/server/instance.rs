//! Instance definition (`instances/<name>/instance.toml`) and server launch arguments.
use super::features::{Edition, Feature};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io;
use std::net::IpAddr;
use std::path::Path;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ZkTarget { pub ensemble: String, pub local: bool }

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SaslConfig { pub acl_group: String, pub user: Option<String> }

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ReplConfig { pub group: String, pub listen: String }

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct InstanceConfig {
    pub name: String,
    pub edition: Edition,
    pub version: String,
    pub features: BTreeSet<Feature>,
    pub build_key: String,
    pub port: u16,
    pub memory_mb: u32,
    pub zk: Option<ZkTarget>,
    pub service_code: Option<String>,
    pub sasl: Option<SaslConfig>,
    pub repl: Option<ReplConfig>,
}

impl InstanceConfig {
    pub fn load(path: &Path) -> io::Result<InstanceConfig> {
        let text = std::fs::read_to_string(path)?;
        toml::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", path.display())))
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() { std::fs::create_dir_all(dir)?; }
        let text = toml::to_string_pretty(self).map_err(io::Error::other)?;
        std::fs::write(path, text)
    }
}

/// Command-line arguments for `<prefix>/bin/memcached`.
pub fn server_args(cfg: &InstanceConfig, prefix: &Path) -> Vec<String> {
    let p = prefix.display();
    let mut a: Vec<String> = vec![
        "-E".into(), format!("{p}/lib/default_engine.so"),
        "-X".into(), format!("{p}/lib/userlog_logger.so"),
        "-p".into(), cfg.port.to_string(),
        "-m".into(), cfg.memory_mb.to_string(),
    ];
    if let Some(z) = &cfg.zk { a.push("-z".into()); a.push(z.ensemble.clone()); }
    if cfg.sasl.is_some() { a.push("-S".into()); }
    a
}

/// The address the cache server registers in `cache_server_mapping`: `ARCUS_CACHE_PUBLIC_IP` if set,
/// else the local address of a UDP socket "connected" towards the first ensemble host (no packet is sent).
pub fn local_ip_for(ensemble: &str) -> io::Result<IpAddr> {
    use std::net::{ToSocketAddrs, UdpSocket};
    if let Some(v) = std::env::var_os("ARCUS_CACHE_PUBLIC_IP").filter(|v| !v.is_empty()) {
        return v.to_string_lossy().parse().map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "ARCUS_CACHE_PUBLIC_IP is not an IP address"));
    }
    let first = ensemble.split(',').next().unwrap_or("");
    let hostport = first.split('/').next().unwrap_or(""); /* drop a chroot suffix */
    let target = hostport.to_socket_addrs()?.next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("cannot resolve `{hostport}`")))?;
    let bind = if target.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
    let sock = UdpSocket::bind(bind)?;
    sock.connect(target)?;
    Ok(sock.local_addr()?.ip())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    pub fn cfg_sasl_local() -> InstanceConfig {
        InstanceConfig {
            name: "dev".into(), edition: Edition::Ee, version: "0.12.1".into(),
            features: BTreeSet::from([Feature::Zk, Feature::Sasl]),
            build_key: "ee-0.12.1-zk_sasl".into(), port: 11211, memory_mb: 64,
            zk: Some(ZkTarget { ensemble: "127.0.0.1:2181".into(), local: true }),
            service_code: Some("svc1".into()),
            sasl: Some(SaslConfig { acl_group: "grp".into(), user: Some("dev".into()) }),
            repl: None,
        }
    }

    #[test]
    fn server_args_full() {
        assert_eq!(server_args(&cfg_sasl_local(), Path::new("/b")),
            ["-E","/b/lib/default_engine.so","-X","/b/lib/userlog_logger.so","-p","11211","-m","64","-z","127.0.0.1:2181","-S"]);
    }

    #[test]
    fn server_args_standalone() {
        let mut c = cfg_sasl_local();
        c.zk = None; c.sasl = None;
        assert_eq!(server_args(&c, Path::new("/b")).len(), 8);
    }

    #[test]
    fn toml_roundtrip() {
        let dir = std::env::temp_dir().join(format!("arcus-inst-{}", std::process::id()));
        let p = dir.join("instance.toml");
        for c in [cfg_sasl_local(), { let mut c = cfg_sasl_local(); c.zk = None; c.sasl = None; c.service_code = None;
            c.repl = Some(ReplConfig { group: "g1".into(), listen: "10.0.0.1:20211".into() }); c }] {
            c.save(&p).unwrap();
            assert_eq!(InstanceConfig::load(&p).unwrap(), c);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_ip_for_loopback() {
        if std::env::var_os("ARCUS_CACHE_PUBLIC_IP").is_none() {
            assert_eq!(local_ip_for("127.0.0.1:2181,127.0.0.1:2182/chroot").unwrap().to_string(), "127.0.0.1");
        }
    }
}
