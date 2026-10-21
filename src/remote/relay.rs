//! EE installs on other machines: this computer (the controller) fetches the private EE source with its own
//! GitHub access, copies the tarball to the machine over scp, and forwards `arcus_install` with `source_file`.
//! Remote machines never need gh or a GitHub token.

use crate::mcp::{Ctx, ToolResult};
use crate::remote::forward::{forward_with, ssh_config_path, ssh_error_result, unknown_host_key};
use crate::remote::machines::resolve;
use crate::remote::ssh::{HostKeyPolicy, Runner, Ssh, SshError};
use crate::server::features::Edition;
use crate::server::home::Home;
use crate::server::release::{ephemeral_gh_dir, fetch_source};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// The EE source tarball downloaded on this computer.
pub struct EeSource {
    pub version: String,
    pub asset_name: String,
    pub path: PathBuf,
    /// The GitHub login was temporary (no keyring) and has been removed after the download.
    pub temporary_login: bool,
}

/// Fetches the EE source of `version` (default: the newest) on this computer.
pub type FetchEe<'a> = &'a dyn Fn(&Home, Option<&str>) -> Result<EeSource, String>;

/// The real `FetchEe`: resolve the version and download with this computer's token (cached in `downloads/`).
pub fn fetch_ee(home: &Home, version: Option<&str>) -> Result<EeSource, String> {
    let temporary_login = ephemeral_gh_dir().is_some();
    let (rel, path) = fetch_source(home, Edition::Ee, version)?;
    Ok(EeSource { version: rel.version, asset_name: rel.asset_name, path, temporary_login })
}

/// Remote directory (relative to the remote home, where scp and `--home ~/.arcus-cli` both start).
const REMOTE_DOWNLOADS: &str = ".arcus-cli/downloads";

fn ssh_failure(machine: &str, e: SshError) -> ToolResult {
    match e {
        SshError::HostKeyUnknown { fingerprint } => unknown_host_key(machine, &fingerprint),
        e => ssh_error_result(e),
    }
}

/// `arcus_install {edition: "ee"}` for `machine`. `args` are the tool arguments without `machine`;
/// features are already validated by the caller.
pub fn relay_ee_install(ctx: &Ctx, args: &Value, machine: &str, runner: &dyn Runner, fetch: FetchEe) -> ToolResult {
    relay_with(ctx, args, machine, runner, &ssh_config_path(), fetch)
}

pub(crate) fn relay_with(ctx: &Ctx, args: &Value, machine: &str, runner: &dyn Runner, config: &Path, fetch: FetchEe) -> ToolResult {
    let m = match resolve(&ctx.home, machine, config) { Ok(m) => m, Err(e) => return ToolResult::err(e) };
    let version = args.get("version").and_then(Value::as_str).filter(|v| !v.is_empty());
    let src = match fetch(&ctx.home, version) {
        Ok(s) => s,
        Err(e) => return ToolResult::err(format!(
            "the EE source is downloaded by this computer and copied to `{machine}`, and that download failed: {e}")),
    };
    if src.asset_name.is_empty() || !src.asset_name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')) {
        return ToolResult::err(format!("unexpected EE asset name `{}`", src.asset_name));
    }
    let ssh = Ssh { home: &ctx.home, runner, machine: &m, policy: HostKeyPolicy::Strict };
    match ssh.exec(&format!("mkdir -p ~/{REMOTE_DOWNLOADS}"), b"") {
        Ok(o) if o.status == 0 => {}
        Ok(o) => return ToolResult::err(format!("cannot create ~/{REMOTE_DOWNLOADS} on `{machine}` ({}): {}", o.status, o.stderr.trim())),
        Err(e) => return ssh_failure(machine, e),
    }
    if let Err(e) = ssh.copy(&src.path, &format!("{REMOTE_DOWNLOADS}/{}", src.asset_name)) {
        return match e {
            SshError::Remote { status, stderr } => ToolResult::err(format!(
                "copying {} to `{machine}` failed ({status}): {}", src.asset_name, stderr.trim())),
            e => ssh_failure(machine, e),
        };
    }
    let mut fwd = args.clone();
    fwd["version"] = json!(src.version);
    fwd["source_file"] = json!(format!("~/{REMOTE_DOWNLOADS}/{}", src.asset_name));
    let mut res = forward_with(ctx, "arcus_install", &fwd, machine, runner, config);
    res.text.push_str(&format!("\n(The EE source {} was downloaded by this computer and copied to `{machine}`; \
        `{machine}` needs no GitHub login.)", src.asset_name));
    if src.temporary_login {
        res.text.push_str(" Tell the user: the GitHub login on this computer was temporary (no keyring) and has been removed; \
            the next EE install will ask for arcus_ee_login again.");
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::ssh::SshOutput;
    use std::cell::RefCell;
    use std::io;

    type Call = (String, Vec<String>, Vec<u8>);
    struct Fake { calls: RefCell<Vec<Call>>, reply: RefCell<Vec<SshOutput>> }
    impl Runner for Fake {
        fn run(&self, p: &str, a: &[String], i: &[u8], _e: &[(String, String)]) -> io::Result<SshOutput> {
            self.calls.borrow_mut().push((p.into(), a.to_vec(), i.to_vec()));
            let mut r = self.reply.borrow_mut();
            Ok(if r.is_empty() { SshOutput { status: 0, stdout: vec![], stderr: String::new() } } else { r.remove(0) })
        }
    }
    fn ok(stdout: &str) -> SshOutput { SshOutput { status: 0, stdout: stdout.as_bytes().to_vec(), stderr: String::new() } }

    fn setup(tag: &str) -> (Ctx, PathBuf, PathBuf) {
        let d = std::env::temp_dir().join(format!("arcus-relay-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let cfg = d.join("config");
        std::fs::write(&cfg, "Host dev-1\n  HostName 10.0.0.1\n").unwrap();
        let tarball = d.join("arcus-memcached-EE-0.12.1.tar.gz");
        std::fs::write(&tarball, b"tar").unwrap();
        (Ctx { home: Home { root: d.join("home") } }, cfg, tarball)
    }

    #[test]
    fn copies_tarball_then_forwards_source_file() {
        let (ctx, cfg, tarball) = setup("ok");
        let asked = RefCell::new(None::<Option<String>>);
        let fetch = |_: &Home, v: Option<&str>| -> Result<EeSource, String> {
            *asked.borrow_mut() = Some(v.map(str::to_string));
            Ok(EeSource { version: "0.12.1".into(), asset_name: "arcus-memcached-EE-0.12.1.tar.gz".into(), path: tarball.clone(), temporary_login: false })
        };
        let f = Fake { calls: RefCell::new(vec![]), reply: RefCell::new(vec![
            ok(""), ok(""),
            ok("{\"arcus_call\":1,\"tag\":\"dev\",\"result\":{\"text\":\"Started build\",\"structured\":{\"job_id\":\"install-1\"},\"is_error\":false}}\n"),
        ]) };
        let r = relay_with(&ctx, &json!({"edition": "ee", "features": ["zk"]}), "dev-1", &f, &cfg, &fetch);
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(*asked.borrow(), Some(None)); // no version given: the controller resolves the newest
        let s = r.structured.unwrap();
        assert!(s["job_id"] == "install-1" && s["machine"] == "dev-1");
        let calls = f.calls.borrow();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].0, "ssh");
        assert_eq!(calls[0].1.last().unwrap(), "mkdir -p ~/.arcus-cli/downloads");
        assert_eq!(calls[1].0, "scp");
        assert_eq!(&calls[1].1[calls[1].1.len() - 2..],
            [tarball.display().to_string(), "dev-1:.arcus-cli/downloads/arcus-memcached-EE-0.12.1.tar.gz".to_string()]);
        assert_eq!(calls[2].0, "ssh");
        assert!(calls[2].1.last().unwrap().contains("--call arcus_install"));
        let sent: Value = serde_json::from_slice(&calls[2].2).unwrap();
        assert_eq!(sent, json!({"edition": "ee", "features": ["zk"], "version": "0.12.1",
            "source_file": "~/.arcus-cli/downloads/arcus-memcached-EE-0.12.1.tar.gz"}));
        let all = String::from_utf8_lossy(&calls[2].2).to_lowercase();
        assert!(!all.contains("token") && !all.contains("machine"), "{all}");
        assert!(r.text.contains("needs no GitHub login"), "{}", r.text);
    }

    #[test]
    fn download_failure_stops_before_ssh() {
        let (ctx, cfg, _) = setup("fail");
        let fetch = |_: &Home, _: Option<&str>| -> Result<EeSource, String> { Err("no GitHub token found".into()) };
        let f = Fake { calls: RefCell::new(vec![]), reply: RefCell::new(vec![]) };
        let r = relay_with(&ctx, &json!({"edition": "ee", "features": [], "version": "0.12.1"}), "dev-1", &f, &cfg, &fetch);
        assert!(r.is_error && r.text.contains("downloaded by this computer") && r.text.contains("no GitHub token"), "{}", r.text);
        assert!(f.calls.borrow().is_empty());
    }

    #[test]
    fn scp_failure_is_reported() {
        let (ctx, cfg, tarball) = setup("scp");
        let fetch = |_: &Home, _: Option<&str>| Ok(EeSource { version: "0.12.1".into(),
            asset_name: "arcus-memcached-EE-0.12.1.tar.gz".into(), path: tarball.clone(), temporary_login: true });
        let f = Fake { calls: RefCell::new(vec![]), reply: RefCell::new(vec![
            ok(""), SshOutput { status: 1, stdout: vec![], stderr: "scp: disk full".into() }]) };
        let r = relay_with(&ctx, &json!({"edition": "ee", "features": []}), "dev-1", &f, &cfg, &fetch);
        assert!(r.is_error && r.text.contains("disk full"), "{}", r.text);
        assert_eq!(f.calls.borrow().len(), 2);
    }
}
