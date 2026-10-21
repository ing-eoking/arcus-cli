//! EE installs on other machines: this computer (the controller) fetches the private EE source with its own
//! GitHub access, copies the tarball to the machine over scp, and forwards `arcus_install` with `source_file`.
//! Remote machines never need gh or a GitHub token.

use crate::mcp::{Ctx, ToolResult};
use crate::remote::forward::{forward_with, ssh_config_path, ssh_error_result, unknown_host_key};
use crate::remote::machines::resolve;
use crate::remote::ssh::{HostKeyPolicy, Runner, Ssh, SshError};
use crate::server::features::Edition;
use crate::server::release::fetch_source;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// The EE source tarball downloaded on this computer.
pub struct EeSource {
    pub version: String,
    pub asset_name: String,
    pub path: PathBuf,
}

/// Fetches the EE source of `version` (default: the newest) on this computer.
pub type FetchEe<'a> = &'a dyn Fn(&Ctx, Option<&str>) -> Result<EeSource, String>;

/// The real `FetchEe`: resolve the version and download with this computer's token (cached in `downloads/`;
/// a given version that is already cached needs no token).
pub fn fetch_ee(ctx: &Ctx, version: Option<&str>) -> Result<EeSource, String> {
    let (rel, path) = fetch_source(&ctx.home, Edition::Ee, version, ctx.gh_session_dir.as_deref())?;
    Ok(EeSource { version: rel.version, asset_name: rel.asset_name, path })
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
    let src = match fetch(ctx, version) {
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
    /* Copy to a `.part.<pid>` file unique to this controller process, then move it into place,
       so the remote never sees a partial tarball under the final name. */
    let dest = format!("{REMOTE_DOWNLOADS}/{}", src.asset_name);
    let part = format!("{dest}.part.{}", std::process::id());
    if let Err(e) = ssh.copy(&src.path, &part) {
        let _ = ssh.exec(&format!("rm -f ~/{part}"), b"");
        return match e {
            SshError::Remote { status, stderr } => ToolResult::err(format!(
                "copying {} to `{machine}` failed ({status}): {}", src.asset_name, stderr.trim())),
            e => ssh_failure(machine, e),
        };
    }
    match ssh.exec(&format!("mv -f ~/{part} ~/{dest} || {{ rm -f ~/{part}; exit 1; }}"), b"") {
        Ok(o) if o.status == 0 => {}
        Ok(o) => return ToolResult::err(format!("cannot move {} into place on `{machine}` ({}): {}", src.asset_name, o.status, o.stderr.trim())),
        Err(e) => return ssh_failure(machine, e),
    }
    let mut fwd = args.clone();
    fwd["version"] = json!(src.version);
    fwd["source_file"] = json!(format!("~/{REMOTE_DOWNLOADS}/{}", src.asset_name));
    let mut res = forward_with(ctx, "arcus_install", &fwd, machine, runner, config);
    res.text.push_str(&format!("\n(The EE source {} was downloaded by this computer and copied to `{machine}`; \
        `{machine}` needs no GitHub login.)", src.asset_name));
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::ssh::SshOutput;
    use crate::server::home::Home;
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
        (Ctx::new(Home { root: d.join("home") }), cfg, tarball)
    }

    #[test]
    fn copies_tarball_then_forwards_source_file() {
        let (ctx, cfg, tarball) = setup("ok");
        let asked = RefCell::new(None::<Option<String>>);
        let fetch = |_: &Ctx, v: Option<&str>| -> Result<EeSource, String> {
            *asked.borrow_mut() = Some(v.map(str::to_string));
            Ok(EeSource { version: "0.12.1".into(), asset_name: "arcus-memcached-EE-0.12.1.tar.gz".into(), path: tarball.clone() })
        };
        let f = Fake { calls: RefCell::new(vec![]), reply: RefCell::new(vec![
            ok(""), ok(""), ok(""),
            ok("{\"arcus_call\":1,\"tag\":\"dev\",\"result\":{\"text\":\"Started build\",\"structured\":{\"job_id\":\"install-1\"},\"is_error\":false}}\n"),
        ]) };
        let r = relay_with(&ctx, &json!({"edition": "ee", "features": ["zk"]}), "dev-1", &f, &cfg, &fetch);
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(*asked.borrow(), Some(None)); // no version given: the controller resolves the newest
        let s = r.structured.unwrap();
        assert!(s["job_id"] == "install-1" && s["machine"] == "dev-1");
        let calls = f.calls.borrow();
        assert_eq!(calls.len(), 4);
        assert_eq!(calls[0].0, "ssh");
        assert_eq!(calls[0].1.last().unwrap(), "mkdir -p ~/.arcus-cli/downloads");
        assert_eq!(calls[1].0, "scp");
        assert_eq!(&calls[1].1[calls[1].1.len() - 2..],
            [tarball.display().to_string(), format!("dev-1:.arcus-cli/downloads/arcus-memcached-EE-0.12.1.tar.gz.part.{}", std::process::id())]);
        let a = ".arcus-cli/downloads/arcus-memcached-EE-0.12.1.tar.gz";
        let p = format!("{a}.part.{}", std::process::id());
        assert_eq!(calls[2].0, "ssh");
        assert_eq!(*calls[2].1.last().unwrap(), format!("mv -f ~/{p} ~/{a} || {{ rm -f ~/{p}; exit 1; }}"));
        assert_eq!(calls[3].0, "ssh");
        assert!(calls[3].1.last().unwrap().contains("--call arcus_install"));
        let sent: Value = serde_json::from_slice(&calls[3].2).unwrap();
        assert_eq!(sent, json!({"edition": "ee", "features": ["zk"], "version": "0.12.1",
            "source_file": "~/.arcus-cli/downloads/arcus-memcached-EE-0.12.1.tar.gz"}));
        let all = String::from_utf8_lossy(&calls[3].2).to_lowercase();
        assert!(!all.contains("token") && !all.contains("machine"), "{all}");
        assert!(r.text.contains("needs no GitHub login"), "{}", r.text);
    }

    #[test]
    fn download_failure_stops_before_ssh() {
        let (ctx, cfg, _) = setup("fail");
        let fetch = |_: &Ctx, _: Option<&str>| -> Result<EeSource, String> { Err("no GitHub token found".into()) };
        let f = Fake { calls: RefCell::new(vec![]), reply: RefCell::new(vec![]) };
        let r = relay_with(&ctx, &json!({"edition": "ee", "features": [], "version": "0.12.1"}), "dev-1", &f, &cfg, &fetch);
        assert!(r.is_error && r.text.contains("downloaded by this computer") && r.text.contains("no GitHub token"), "{}", r.text);
        assert!(f.calls.borrow().is_empty());
    }

    #[test]
    fn scp_failure_is_reported() {
        let (ctx, cfg, tarball) = setup("scp");
        let fetch = |_: &Ctx, _: Option<&str>| Ok(EeSource { version: "0.12.1".into(),
            asset_name: "arcus-memcached-EE-0.12.1.tar.gz".into(), path: tarball.clone() });
        let f = Fake { calls: RefCell::new(vec![]), reply: RefCell::new(vec![
            ok(""), SshOutput { status: 1, stdout: vec![], stderr: "scp: disk full".into() }]) };
        let r = relay_with(&ctx, &json!({"edition": "ee", "features": []}), "dev-1", &f, &cfg, &fetch);
        assert!(r.is_error && r.text.contains("disk full"), "{}", r.text);
        let calls = f.calls.borrow();
        assert_eq!(calls.len(), 3);
        assert_eq!(*calls[2].1.last().unwrap(), format!("rm -f ~/.arcus-cli/downloads/arcus-memcached-EE-0.12.1.tar.gz.part.{}", std::process::id()));
    }

    #[test]
    fn failed_move_is_reported_and_not_forwarded() {
        let (ctx, cfg, tarball) = setup("mv");
        let fetch = |_: &Ctx, _: Option<&str>| Ok(EeSource { version: "0.12.1".into(),
            asset_name: "arcus-memcached-EE-0.12.1.tar.gz".into(), path: tarball.clone() });
        let f = Fake { calls: RefCell::new(vec![]), reply: RefCell::new(vec![
            ok(""), ok(""), SshOutput { status: 1, stdout: vec![], stderr: "mv: read-only".into() }]) };
        let r = relay_with(&ctx, &json!({"edition": "ee", "features": []}), "dev-1", &f, &cfg, &fetch);
        assert!(r.is_error && r.text.contains("read-only"), "{}", r.text);
        assert_eq!(f.calls.borrow().len(), 3);
    }
}
