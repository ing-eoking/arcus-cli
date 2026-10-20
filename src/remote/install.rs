//! `arcus_host_install`: ship an arcus-cli binary to a machine and keep the resident agent
//! running there (systemd user unit / launchd agent / plain daemon), behind a plan/apply gate.
//! All remote steps go through `Runner`, so the flow is testable without ssh.

use crate::mcp::remote_tools::host_check;
use crate::mcp::{arg_bool, arg_opt_str, arg_str, ToolResult};
use crate::remote::dist::choose_binary;
use crate::remote::forward::ssh_error_result;
use crate::remote::machines::resolve;
use crate::remote::ssh::{HostKeyPolicy, Runner, Ssh};
use crate::server::home::Home;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

const REMOTE_PATH: &str = "~/.arcus-cli/bin/arcus-cli";
const BIN_DIR: &str = "$HOME/.arcus-cli/bin";
const BIN: &str = "$HOME/.arcus-cli/bin/arcus-cli";
const UNIT_PATH: &str = "$HOME/.config/systemd/user/arcus-cli-agent.service";
const PLIST_PATH: &str = "$HOME/Library/LaunchAgents/com.arcus-cli.agent.plist";
const POLL_TRIES: u32 = 10;

pub fn systemd_unit(bin: &str, home: &str) -> String {
    format!("[Unit]\nDescription=arcus-cli agent\n\n[Service]\nExecStart={bin} --agent --home {home}\n\
Restart=on-failure\nKillMode=process\n\n[Install]\nWantedBy=default.target\n")
}

pub fn launchd_plist(bin: &str, home: &str) -> String {
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n<dict>\n\
  <key>Label</key><string>com.arcus-cli.agent</string>\n\
  <key>ProgramArguments</key>\n  <array>\n    <string>{bin}</string>\n    <string>--agent</string>\n    <string>--home</string>\n    <string>{home}</string>\n  </array>\n\
  <key>RunAtLoad</key><true/>\n\
  <key>KeepAlive</key>\n  <dict><key>SuccessfulExit</key><false/></dict>\n\
  <key>AbandonProcessGroup</key><true/>\n\
  <key>StandardOutPath</key><string>{home}/agent.log</string>\n\
  <key>StandardErrorPath</key><string>{home}/agent.log</string>\n\
</dict>\n</plist>\n")
}

/// Linux form (`setsid` detaches from the ssh session).
pub fn daemon_command(bin: &str, home: &str) -> String {
    daemon_command_for("Linux", bin, home)
}

/// macOS has no `setsid` binary, so it is omitted there.
pub fn daemon_command_for(os: &str, bin: &str, home: &str) -> String {
    let setsid = if os == "Darwin" { "" } else { "setsid " };
    format!("{setsid}nohup {bin} --agent --home {home} >> {home}/agent.log 2>&1 < /dev/null &")
}

fn stop_old_agent(mode: &str, os: &str) -> String {
    match (mode, os) {
        ("service", "Darwin") => "launchctl bootout gui/$(id -u)/com.arcus-cli.agent".to_string(),
        ("service", _) => "systemctl --user stop arcus-cli-agent".to_string(),
        _ => "pid=$(cat \"$HOME/.arcus-cli/agent.pid\") && kill \"$pid\"; \
for i in 1 2 3 4 5; do kill -0 \"$pid\" 2>/dev/null || break; sleep 1; done; true".to_string(),
    }
}

fn plan_steps(os: &str, mode: &str, replace: bool, has_agent: bool) -> Vec<String> {
    let mut s = vec![
        format!("copy the arcus-cli binary to {REMOTE_PATH}.new, chmod 755, then atomically rename it to {REMOTE_PATH}"),
    ];
    if mode == "service" && os == "Linux" {
        s.insert(0, "check `loginctl show-user $USER -p Linger`; if it is `no`, run `loginctl enable-linger` \
(if refused, stop and offer daemon mode or an admin-enabled linger)".into());
    }
    if replace && has_agent {
        s.push(format!("stop only the old agent (`{}`); supervisors and servers keep running", stop_old_agent(mode, os)));
    }
    s.push(match (mode, os) {
        ("service", "Darwin") => "write ~/Library/LaunchAgents/com.arcus-cli.agent.plist and run `launchctl bootstrap gui/$(id -u) <plist>`".to_string(),
        ("service", _) => "write ~/.config/systemd/user/arcus-cli-agent.service (KillMode=process) and run \
`systemctl --user daemon-reload` and `systemctl --user enable --now arcus-cli-agent`".to_string(),
        _ => format!("start the agent as a daemon: `{}`", daemon_command_for(os, REMOTE_PATH, "~/.arcus-cli")),
    });
    s.push("poll `arcus-cli --agent-status` for up to 10 s, then report the machine state".into());
    s
}

fn linger_refused(user: &str, detail: &str) -> ToolResult {
    ToolResult::err(format!(
        "could not enable linger ({}). Without linger a systemd user service stops when the user logs out, so the cluster \
would stop after logout. Nothing was installed. Ask the user to choose: (1) call again with mode: \"daemon\", or \
(2) ask an administrator to run `sudo loginctl enable-linger {user}` and then call again. Never run sudo yourself",
        detail.trim()))
}

fn safe_home(h: &str) -> bool {
    !h.is_empty() && h.starts_with('/') && h.chars().all(|c| c.is_ascii_alphanumeric() || "/_.-".contains(c))
}

pub fn host_install(home: &Home, runner: &dyn Runner, config: &Path, args: &Value, poll: Duration) -> ToolResult {
    let name = match arg_str(args, "machine") { Ok(v) => v, Err(r) => return r };
    let mode = arg_opt_str(args, "mode").unwrap_or_else(|| "service".into());
    if mode != "service" && mode != "daemon" {
        return ToolResult::err(format!("invalid mode `{mode}`: use \"service\" (recommended) or \"daemon\""));
    }
    let replace = arg_bool(args, "replace", false);
    let apply = arg_bool(args, "apply", false);

    let check = host_check(home, runner, config, args);
    if check.is_error { return check; }
    let data = check.structured.clone().unwrap_or_else(|| json!({}));
    let os = data["os"].as_str().unwrap_or("").to_string();
    let Some(target) = data["target"].as_str().map(str::to_string) else {
        return ToolResult::err(format!("`{name}` ({os} {}) is not a supported target", data["arch"].as_str().unwrap_or("?")));
    };
    let rhome = data["home"].as_str().unwrap_or("").to_string();
    let has_agent = !data["agent"].is_null();
    if has_agent && !replace {
        let version = data["arcus_cli"]["version"].as_str().unwrap_or("unknown");
        let tag = data["arcus_cli"]["tag"].as_str().unwrap_or("");
        return ToolResult::err(format!(
            "an arcus-cli agent is already running on `{name}` (version {version} {tag}), managing servers: {}. \
Ask the user whether to replace it; if yes call again with replace: true. Replacing stops only the agent; \
supervisors and servers keep running",
            data["agent"]["servers"]));
    }
    let binary_arg = arg_opt_str(args, "binary");
    let tag_arg = arg_opt_str(args, "tag");
    let steps = plan_steps(&os, &mode, replace, has_agent);
    if !apply {
        let source = match (&binary_arg, &tag_arg) {
            (Some(b), _) => format!("local file {b}"),
            (None, Some(t)) => format!("release {t} ({target})"),
            (None, None) => format!("release {} ({target})", crate::TAG),
        };
        let tag = binary_arg.as_ref().map(|_| "custom".to_string())
            .or(tag_arg.clone()).unwrap_or_else(|| crate::TAG.to_string());
        let plan = json!({"machine": name, "binary_source": source, "tag": tag, "remote_path": REMOTE_PATH,
            "mode": mode, "replace": replace, "steps": steps});
        return ToolResult::ok(
            format!("plan for `{name}` (nothing changed). Show it to the user and call again with apply: true after they agree:\n- {}", steps.join("\n- ")),
            Some(plan));
    }
    if !safe_home(&rhome) {
        return ToolResult::err(format!("unsupported remote home directory `{rhome}`"));
    }

    let machine = match resolve(home, &name, config) { Ok(m) => m, Err(e) => return ToolResult::err(e) };
    let policy = if arg_bool(args, "accept_host_key", false) { HostKeyPolicy::AcceptNew } else { HostKeyPolicy::Strict };
    let ssh = Ssh { home, runner, machine: &machine, policy };
    let run = |cmd: &str, stdin: &[u8]| -> Result<crate::remote::ssh::SshOutput, ToolResult> {
        ssh.exec(cmd, stdin).map_err(ssh_error_result)
    };
    let must = |cmd: &str, stdin: &[u8], what: &str| -> Result<(), ToolResult> {
        let o = run(cmd, stdin)?;
        if o.status == 0 { Ok(()) } else {
            Err(ToolResult::err(format!("{what} failed ({}): {}", o.status, o.stderr.trim())))
        }
    };

    macro_rules! t { ($e:expr) => { match $e { Ok(v) => v, Err(r) => return r } }; }

    /* Linger first: nothing is changed on the machine if it is refused. */
    if mode == "service" && os == "Linux" {
        let o = t!(run("loginctl show-user \"$(id -un)\" -p Linger; id -un", b""));
        let text = String::from_utf8_lossy(&o.stdout).into_owned();
        if !text.contains("Linger=yes") {
            let user = text.lines().rfind(|l| !l.starts_with("Linger=")).unwrap_or("<user>").trim().to_string();
            let e = t!(run("loginctl enable-linger", b""));
            if e.status != 0 { return linger_refused(&user, &e.stderr); }
        }
    }

    let (local, used_tag) = match choose_binary(home, &target, tag_arg.as_deref(), binary_arg.as_deref().map(Path::new)) {
        Ok(v) => v,
        Err(e) => return ToolResult::err(e),
    };
    t!(must(&format!("mkdir -p \"{BIN_DIR}\" \"$HOME/.arcus-cli\""), b"", "creating ~/.arcus-cli/bin"));
    if let Err(e) = ssh.copy(&local, ".arcus-cli/bin/arcus-cli.new") { return ssh_error_result(e); }
    t!(must(&format!("chmod 755 \"{BIN}.new\" && mv -f \"{BIN}.new\" \"{BIN}\""), b"", "installing the binary"));

    if replace && has_agent {
        /* Only the agent: supervisors and servers are separate processes and keep running. */
        t!(must(&stop_old_agent(&mode, &os), b"", "stopping the old agent"));
    }

    let bin = format!("{rhome}/.arcus-cli/bin/arcus-cli");
    let ahome = format!("{rhome}/.arcus-cli");
    match (mode.as_str(), os.as_str()) {
        ("service", "Darwin") => {
            t!(must(&format!("mkdir -p \"$HOME/Library/LaunchAgents\" && cat > \"{PLIST_PATH}\""),
                launchd_plist(&bin, &ahome).as_bytes(), "writing the launchd plist"));
            t!(must(&format!("launchctl bootstrap gui/$(id -u) \"{PLIST_PATH}\""), b"", "launchctl bootstrap"));
        }
        ("service", _) => {
            t!(must(&format!("mkdir -p \"$HOME/.config/systemd/user\" && cat > \"{UNIT_PATH}\""),
                systemd_unit(&bin, &ahome).as_bytes(), "writing the systemd unit"));
            t!(must("systemctl --user daemon-reload && systemctl --user enable --now arcus-cli-agent", b"", "starting the service"));
        }
        _ => {
            t!(must(&daemon_command_for(&os, &bin, &ahome), b"", "starting the daemon"));
        }
    }

    let status_cmd = format!("\"{BIN}\" --agent-status --home \"$HOME/.arcus-cli\"");
    let mut up = false;
    for i in 0..POLL_TRIES {
        if i > 0 { std::thread::sleep(poll); }
        if let Ok(o) = ssh.exec(&status_cmd, b"") {
            let text = String::from_utf8_lossy(&o.stdout);
            if o.status == 0 && text.lines().filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
                .any(|v| v["running"].as_bool().unwrap_or(false)) { up = true; break; }
        }
    }
    if !up {
        return ToolResult::err(format!(
            "the agent was installed on `{name}` but did not report running within 10 s; check ~/.arcus-cli/agent.log on the machine"));
    }
    let mut after = host_check(home, runner, config, args);
    if let Some(s) = after.structured.as_mut() { s["installed_tag"] = json!(used_tag); }
    after.text = format!("installed arcus-cli agent ({used_tag}) on `{name}` in {mode} mode. {}", after.text);
    after
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::ssh::SshOutput;
    use std::cell::RefCell;
    use std::io;
    use std::path::PathBuf;

    struct Fake { calls: RefCell<Vec<(String, Vec<String>)>>, rules: Vec<(&'static str, SshOutput)> }
    impl Runner for Fake {
        fn run(&self, p: &str, a: &[String], _i: &[u8], _e: &[(String, String)]) -> io::Result<SshOutput> {
            self.calls.borrow_mut().push((p.into(), a.to_vec()));
            let last = a.last().cloned().unwrap_or_default();
            for (pat, o) in &self.rules {
                if last.contains(pat) {
                    return Ok(SshOutput { status: o.status, stdout: o.stdout.clone(), stderr: o.stderr.clone() });
                }
            }
            Ok(SshOutput { status: 0, stdout: vec![], stderr: String::new() })
        }
    }
    fn o(status: i32, stdout: &str, stderr: &str) -> SshOutput { SshOutput { status, stdout: stdout.into(), stderr: stderr.into() } }
    fn fake(rules: Vec<(&'static str, SshOutput)>) -> Fake { Fake { calls: RefCell::new(vec![]), rules } }
    fn setup(tag: &str) -> (Home, PathBuf, PathBuf) {
        let d = std::env::temp_dir().join(format!("arcus-inst-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let cfg = d.join("config");
        std::fs::write(&cfg, "Host dev-1\n  HostName 10.0.0.1\n").unwrap();
        let bin = d.join("arcus-cli");
        std::fs::write(&bin, "x").unwrap();
        (Home { root: d.join("home") }, cfg, bin)
    }
    const IDLE: &str = "Linux x86_64\n/home/u\narcus-cli 1.0.0 (v1)\n";
    const RUNNING: &str = "Linux x86_64\n/home/u\narcus-cli 1.0.0 (v1)\n{\"running\":true,\"servers\":[\"svc-a\"]}\n";
    fn cmds(f: &Fake) -> Vec<String> { f.calls.borrow().iter().map(|(_, a)| a.last().cloned().unwrap_or_default()).collect() }

    #[test] fn unit_has_killmode_process() {
        let u = systemd_unit("/b", "/h");
        assert!(u.contains("KillMode=process\n") && u.contains("ExecStart=/b --agent --home /h\n"));
        assert!(u.contains("Restart=on-failure") && u.contains("WantedBy=default.target"));
    }
    #[test] fn plist_abandons_process_group() {
        let p = launchd_plist("/b", "/h");
        assert!(p.contains("com.arcus-cli.agent") && p.contains("<key>AbandonProcessGroup</key><true/>"));
        assert!(p.contains("<key>RunAtLoad</key><true/>") && p.contains("SuccessfulExit"));
    }
    #[test] fn daemon_command_forms() {
        assert_eq!(daemon_command("/b", "/h"), "setsid nohup /b --agent --home /h >> /h/agent.log 2>&1 < /dev/null &");
        assert!(!daemon_command_for("Darwin", "/b", "/h").contains("setsid"));
    }
    #[test] fn install_refuses_running_agent_without_replace() {
        let (home, cfg, _) = setup("running");
        let f = fake(vec![("uname", o(0, RUNNING, ""))]);
        let r = host_install(&home, &f, &cfg, &json!({"machine": "dev-1", "apply": true}), Duration::ZERO);
        assert!(r.is_error && r.text.contains("1.0.0") && r.text.contains("svc-a") && r.text.contains("replace: true"), "{}", r.text);
        assert_eq!(f.calls.borrow().len(), 1);
    }
    #[test] fn install_plan_without_apply_writes_nothing() {
        let (home, cfg, _) = setup("plan");
        let f = fake(vec![("uname", o(0, IDLE, ""))]);
        let r = host_install(&home, &f, &cfg, &json!({"machine": "dev-1", "tag": "v1"}), Duration::ZERO);
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(f.calls.borrow().len(), 1);
        let s = r.structured.unwrap();
        assert_eq!(s["remote_path"], "~/.arcus-cli/bin/arcus-cli");
        assert_eq!(s["mode"], "service");
        assert_eq!(s["tag"], "v1");
        assert!(s["steps"].as_array().unwrap().len() >= 3);
    }
    #[test] fn linger_refused_offers_daemon() {
        let (home, cfg, bin) = setup("linger");
        let f = fake(vec![("uname", o(0, IDLE, "")), ("show-user", o(0, "Linger=no\nu\n", "")),
            ("enable-linger", o(1, "", "Access denied"))]);
        let r = host_install(&home, &f, &cfg,
            &json!({"machine": "dev-1", "apply": true, "binary": bin.to_str().unwrap()}), Duration::ZERO);
        assert!(r.is_error);
        assert!(r.text.contains("mode: \"daemon\"") && r.text.contains("sudo loginctl enable-linger u") && r.text.contains("after logout"), "{}", r.text);
        assert!(!cmds(&f).iter().any(|c| c.contains("mkdir") || c.contains("sudo ")));
        assert!(!f.calls.borrow().iter().any(|(p, _)| p == "scp"));
    }
    #[test] fn replace_daemon_stops_only_agent() {
        let (home, cfg, bin) = setup("replace");
        let f = fake(vec![("uname", o(0, RUNNING, "")), ("--agent-status", o(0, "{\"running\":true}\n", ""))]);
        let r = host_install(&home, &f, &cfg,
            &json!({"machine": "dev-1", "mode": "daemon", "replace": true, "apply": true, "binary": bin.to_str().unwrap()}), Duration::ZERO);
        assert!(!r.is_error, "{}", r.text);
        let c = cmds(&f);
        assert!(c.iter().any(|x| x.contains("kill \"$pid\"") && x.contains("agent.pid")));
        assert!(!c.iter().any(|x| x.contains("pkill") || x.contains("killall") || x.contains("--supervise")));
        assert!(c.iter().any(|x| x.starts_with("setsid nohup /home/u/.arcus-cli/bin/arcus-cli --agent --home /home/u/.arcus-cli")));
        let calls = f.calls.borrow();
        assert!(calls.iter().any(|(p, a)| p == "scp" && a.last().unwrap().ends_with(":.arcus-cli/bin/arcus-cli.new")));
        let mv = c.iter().position(|x| x.contains("mv -f")).unwrap();
        let start = c.iter().position(|x| x.starts_with("setsid")).unwrap();
        assert!(mv < start);
    }
    #[test] fn service_linux_writes_unit_and_enables() {
        let (home, cfg, bin) = setup("svc");
        let f = fake(vec![("uname", o(0, IDLE, "")), ("show-user", o(0, "Linger=yes\nu\n", "")),
            ("--agent-status", o(0, "{\"running\":true}\n", ""))]);
        let r = host_install(&home, &f, &cfg,
            &json!({"machine": "dev-1", "apply": true, "binary": bin.to_str().unwrap()}), Duration::ZERO);
        assert!(!r.is_error, "{}", r.text);
        let c = cmds(&f);
        assert!(c.iter().any(|x| x.contains("arcus-cli-agent.service")));
        assert!(c.iter().any(|x| x.contains("enable --now arcus-cli-agent")));
        assert!(!c.iter().any(|x| x == "loginctl enable-linger"));
    }
}
