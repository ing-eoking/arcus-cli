use std::env;
use std::collections::BTreeSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::features::{configure_args, Feature};
use super::features::Edition;
use super::jobs::{tail, JobCtx};
use super::release::{download_source, find_gh, find_token, list_releases};
use serde_json::{json, Value};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Os {
    Rhel,
    Debian,
    Mac,
    Other,
}

type BuildStep = (&'static str, &'static str, Vec<String>, Vec<(String, String)>);

pub fn detect_os() -> Os {
    if cfg!(target_os = "macos") {
        return Os::Mac;
    }
    let text = std::fs::read_to_string("/etc/os-release").unwrap_or_default().to_lowercase();
    let ids: String = text.lines()
        .filter(|l| l.starts_with("id=") || l.starts_with("id_like="))
        .collect::<Vec<_>>().join(" ");
    if ["rhel", "centos", "fedora", "rocky", "almalinux"].iter().any(|k| ids.contains(k)) {
        Os::Rhel
    } else if ["debian", "ubuntu"].iter().any(|k| ids.contains(k)) {
        Os::Debian
    } else {
        Os::Other
    }
}

/// Returns required build tools for which `have` is false (`libtool` is probed as `glibtool` on macOS).
pub fn missing_tools(have: &dyn Fn(&str) -> bool) -> Vec<&'static str> {
    let mac = cfg!(target_os = "macos");
    ["cc", "make", "libtool", "perl", "autoconf", "automake"]
        .into_iter()
        .filter(|&t| {
            let probe = if mac && t == "libtool" { "glibtool" } else { t };
            !have(probe)
        })
        .collect()
}

/// Whether `name` resolves to an executable file in a PATH entry.
pub fn on_path(name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    env::var_os("PATH").is_some_and(|paths| {
        env::split_paths(&paths).any(|d| {
            std::fs::metadata(Path::new(&d).join(name))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    })
}

/// Install guidance shown to the user; never executed.
pub fn install_hint(os: Os, missing: &[&str]) -> String {
    match os {
        Os::Rhel => "sudo dnf install gcc make libtool perl autoconf automake".into(),
        Os::Debian => "sudo apt-get install build-essential libtool perl autoconf automake".into(),
        Os::Mac => {
            let mut s = String::from("brew install libtool autoconf automake");
            if missing.iter().any(|t| *t == "cc" || *t == "make") {
                s.push_str("\nxcode-select --install");
            }
            s
        }
        Os::Other => format!("Install these with your package manager: {}", missing.join(", ")),
    }
}

/// Runs one build command with stdout/stderr appended to `log`.
pub trait Runner {
    fn run(&mut self, program: &str, args: &[String], env: &[(String, String)], cwd: &Path, log: &Path) -> io::Result<bool>;
}

pub struct RealRunner;

impl Runner for RealRunner {
    fn run(&mut self, program: &str, args: &[String], env: &[(String, String)], cwd: &Path, log: &Path) -> io::Result<bool> {
        let out = std::fs::OpenOptions::new().create(true).append(true).open(log)?;
        let status = Command::new(program).args(args).envs(env.iter().map(|(k, v)| (k, v))).current_dir(cwd)
            .stdin(Stdio::null()).stdout(out.try_clone()?).stderr(out).status()?;
        Ok(status.success())
    }
}

pub(crate) fn extract(tarball: &Path, dest: &Path) -> io::Result<PathBuf> {
    let _ = std::fs::remove_dir_all(dest);
    std::fs::create_dir_all(dest)?;
    let gz = flate2::read::GzDecoder::new(std::fs::File::open(tarball)?);
    tar::Archive::new(gz).unpack(dest)?;
    let mut dirs = std::fs::read_dir(dest)?
        .filter_map(|e| e.ok()).filter(|e| e.path().is_dir()).map(|e| e.path());
    match (dirs.next(), dirs.next()) {
        (Some(d), None) => Ok(d),
        _ => Err(io::Error::new(io::ErrorKind::InvalidData, "source tarball must contain exactly one top-level directory")),
    }
}

fn append_file(log: &Path, file: &Path) {
    if let (Ok(text), Ok(mut f)) = (std::fs::read(file), std::fs::OpenOptions::new().create(true).append(true).open(log)) {
        let _ = f.write_all(&text);
    }
}

/// Builds the source tarball into `prefix`; skipped when `<prefix>/bin/memcached` already exists.
pub fn build(tarball: &Path, prefix: &Path, features: &BTreeSet<Feature>, log: &Path,
             runner: &mut dyn Runner, step: &dyn Fn(&str)) -> io::Result<()> {
    if prefix.join("bin").join("memcached").is_file() { return Ok(()); }
    let src_parent = PathBuf::from(format!("{}.src", prefix.display()));
    let src = extract(tarball, &src_parent)?;
    let fail = |name: &str| io::Error::other(format!(
        "build step `{name}` failed; see {}\n{}", log.display(), tail(log, 20).unwrap_or_default()));
    let p = prefix.display().to_string();
    // cyrus-sasl hardcodes framedir=/Library/Frameworks/...; override it so install stays in the prefix.
    let deps_env: Vec<(String, String)> = if cfg!(target_os = "macos") {
        vec![("MAKEFLAGS".into(), format!("framedir={p}/Frameworks/SASL2.framework"))]
    } else { vec![] };
    let plan: [BuildStep; 4] = [
        ("deps", "./deps/install.sh", vec![p], deps_env),
        ("configure", "./configure", configure_args(features, prefix), vec![]),
        ("make", "make", vec![], vec![]),
        ("make install", "make", vec!["install".to_string()], vec![]),
    ];
    for (name, program, args, env) in plan {
        step(name);
        let ok = runner.run(program, &args, &env, &src, log)?;
        if name == "deps" { append_file(log, &src.join("deps").join("install.log")); }
        if !ok { return Err(fail(name)); }
    }
    Ok(())
}

fn parse_edition(s: &str) -> Result<Edition, String> {
    match s {
        "public" => Ok(Edition::Public),
        "ee" => Ok(Edition::Ee),
        other => Err(format!("invalid edition '{other}': use \"public\" or \"ee\"")),
    }
}

/// Job handler for `JobKind::Install`. Params: `{edition, version, features: [..], build_key}`.
/// The token is looked up here, never stored in params.
pub fn install_job(ctx: &JobCtx, params: &Value) -> Result<Value, String> {
    let edition = parse_edition(params["edition"].as_str().unwrap_or(""))?;
    let version = params["version"].as_str().ok_or("missing version")?.to_string();
    let key = params["build_key"].as_str().ok_or("missing build_key")?.to_string();
    let features: BTreeSet<Feature> = serde_json::from_value(params["features"].clone()).map_err(|e| e.to_string())?;
    let token = if edition == Edition::Ee { find_token(find_gh(&ctx.home).as_deref()) } else { None };
    if edition == Edition::Ee && token.is_none() {
        return Err("no GitHub token found; call arcus_ee_login first".into());
    }
    let prefix = ctx.home.build_prefix(&key);
    if !prefix.join("bin").join("memcached").is_file() {
        ctx.step("download");
        let rel = list_releases(edition, token.as_ref()).map_err(|e| e.to_string())?
            .into_iter().find(|r| r.version == version)
            .ok_or_else(|| format!("version {version} not found"))?;
        let tarball = download_source(&rel, token.as_ref(), &ctx.home).map_err(|e| e.to_string())?;
        build(&tarball, &prefix, &features, &ctx.log, &mut RealRunner, &|s| ctx.step(s)).map_err(|e| e.to_string())?;
    }
    Ok(json!({"build_key": key, "prefix": prefix.display().to_string()}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::home::Home;
    use std::collections::BTreeSet;
    use std::io;
    use std::path::PathBuf;
    use crate::server::features::{configure_args, Feature};

    struct Rec { calls: Vec<(String, Vec<String>, PathBuf)>, envs: Vec<Vec<(String, String)>>, fail_on: Option<&'static str> }
    impl Runner for Rec {
        fn run(&mut self, program: &str, args: &[String], env: &[(String, String)], cwd: &Path, _log: &Path) -> io::Result<bool> {
            self.envs.push(env.to_vec());
            self.calls.push((program.into(), args.to_vec(), cwd.to_path_buf()));
            Ok(self.fail_on != Some(program) || args.first().map(String::as_str) == Some("install"))
        }
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("arcus-cli-build-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn tiny_tarball(dir: &Path) -> PathBuf {
        let path = dir.join("src.tar.gz");
        let gz = flate2::write::GzEncoder::new(std::fs::File::create(&path).unwrap(), flate2::Compression::fast());
        let mut b = tar::Builder::new(gz);
        let data = b"hi";
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, "arcus-memcached-1.0.0/README", &data[..]).unwrap();
        b.into_inner().unwrap().finish().unwrap();
        path
    }

    #[test]
    fn build_runs_steps_in_order() {
        let d = tmp("order");
        let tarball = tiny_tarball(&d);
        let prefix = d.join("prefix");
        let features = BTreeSet::from([Feature::Zk]);
        let mut r = Rec { calls: vec![], envs: vec![], fail_on: None };
        let steps = std::cell::RefCell::new(Vec::new());
        build(&tarball, &prefix, &features, &d.join("job.log"), &mut r, &|s| steps.borrow_mut().push(s.to_string())).unwrap();
        let progs: Vec<_> = r.calls.iter().map(|c| c.0.as_str()).collect();
        assert_eq!(progs, ["./deps/install.sh", "./configure", "make", "make"]);
        assert_eq!(r.calls[1].1, configure_args(&features, &prefix));
        assert_eq!(r.calls[3].1, ["install"]);
        assert!(r.calls[0].2.ends_with("prefix.src/arcus-memcached-1.0.0"));
        for (i, e) in r.envs.iter().enumerate() {
            if i == 0 && cfg!(target_os = "macos") {
                assert_eq!(*e, [("MAKEFLAGS".to_string(), format!("framedir={}/Frameworks/SASL2.framework", prefix.display()))]);
            } else {
                assert!(e.is_empty());
            }
        }
        assert_eq!(*steps.borrow(), ["deps", "configure", "make", "make install"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn build_failure_names_step() {
        let d = tmp("fail");
        let tarball = tiny_tarball(&d);
        let log = d.join("job.log");
        let mut r = Rec { calls: vec![], envs: vec![], fail_on: Some("make") };
        let e = build(&tarball, &d.join("prefix"), &BTreeSet::new(), &log, &mut r, &|_| {}).unwrap_err();
        let m = e.to_string();
        assert!(m.contains("make") && m.contains(&log.display().to_string()), "{m}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn build_skips_when_already_built() {
        let d = tmp("skip");
        let prefix = d.join("prefix");
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        std::fs::write(prefix.join("bin/memcached"), "").unwrap();
        let mut r = Rec { calls: vec![], envs: vec![], fail_on: None };
        build(&d.join("missing.tar.gz"), &prefix, &BTreeSet::new(), &d.join("l"), &mut r, &|_| {}).unwrap();
        assert!(r.calls.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[ignore]
    fn build_public_1_16_1() {
        let root = std::env::temp_dir().join(format!("arcus-cli-real-build-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = Home::new(Some(root.clone())).unwrap();
        let features = BTreeSet::from([Feature::Zk]);
        let key = crate::server::features::build_key(Edition::Public, "1.16.1", &features);
        let log = root.join("job.log");
        let ctx = JobCtx { id: "install-test".into(), home: home.clone(), log: log.clone() };
        let r = install_job(&ctx, &json!({"edition": "public", "version": "1.16.1", "features": ["zk"], "build_key": key}));
        let prefix = home.build_prefix(&key);
        let ok = r.is_ok() && prefix.join("bin/memcached").is_file()
            && prefix.join("lib/default_engine.so").is_file() && prefix.join("lib/userlog_logger.so").is_file();
        if !ok { eprintln!("{r:?}\n{}", tail(&log, 40).unwrap_or_default()); }
        let _ = std::fs::remove_dir_all(&root);
        assert!(ok);
    }

    #[test]
    fn hints() {
        assert!(install_hint(Os::Mac, &["cc"]).contains("xcode-select --install"));
        assert!(install_hint(Os::Rhel, &["libtool"]).contains("dnf install"));
    }
    #[test]
    fn missing_uses_glibtool_on_mac_and_reports_absent() {
        let m = missing_tools(&|t| t != "perl" && t != "make");
        assert_eq!(m, ["make", "perl"]);
    }
}
