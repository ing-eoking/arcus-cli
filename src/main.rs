mod helper;
mod connect;
mod request;
mod zk;
mod mcp;
mod server;

use std::path::PathBuf;
use std::time;
use rustyline::history::DefaultHistory;
use rustyline::Editor;
use rustyline::error::ReadlineError;
use clap::{ArgAction, ArgGroup, Parser};
use connect::Transport;

/// Release tag baked in at build time (`dev` for local builds).
pub const TAG: &str = match option_env!("ARCUS_CLI_TAG") { Some(t) => t, None => "dev" };

#[derive(Parser, Debug)]
#[command(version = env!("ARCUS_CLI_VERSION"), about, long_about = None)]
#[command(group(ArgGroup::new("mode").multiple(true).args(["mcp", "job", "supervise", "call"])))]
struct Args {
    /// Host name or IP or Unix path
    #[arg(long, default_value_t = String::from("127.0.0.1"))]
    host: String,

    /// Port Number (default: 11211, or 2181 in --zookeeper mode)
    #[arg(short, long)]
    port: Option<u16>,

    /// Connect to a ZooKeeper ensemble (zkCli-like), disables memcached mode
    #[clap(long, action=ArgAction::SetTrue)]
    zookeeper: bool,

    /// Use UDP protocol
    #[clap(long, action=ArgAction::SetTrue)]
    udp: bool,

    /// Request ID for UDP
    #[arg(long, default_value_t = 1)]
    req_id: u16,

    /// Use Unix socket (disables network support)
    #[arg(long, action=ArgAction::SetTrue)]
    unix: bool,

    /// Timeout(ms) to wait for responses (UDP replies, and pending replies on exit)
    #[arg(short, long, default_value_t = 1000)]
    timeout: u64,

    /// Authenticate with sasl
    #[clap(long, action=ArgAction::SetTrue)]
    sasl: bool,

    /// Run as an MCP server over stdio (JSON-RPC on stdout, diagnostics on stderr)
    #[arg(long, action=ArgAction::SetTrue,
          conflicts_with_all = ["host", "port", "zookeeper", "udp", "req_id", "unix", "timeout", "sasl"])]
    mcp: bool,

    /// Data directory for --mcp (default: $HOME/.arcus-cli)
    #[arg(long, value_name = "DIR", requires = "mode")]
    home: Option<PathBuf>,

    /// Run an install/start job in the background (internal)
    #[arg(long, hide = true, value_name = "ID")]
    job: Option<String>,

    /// Run one tool and print its result as a single JSON line (internal)
    #[arg(long, hide = true, value_name = "TOOL")]
    call: Option<String>,

    /// Supervise a server instance (internal)
    #[arg(long, hide = true, value_name = "NAME")]
    supervise: Option<String>,
}

fn main() -> rustyline::Result<()> {
    let args = Args::parse();

    if args.mcp || args.job.is_some() || args.call.is_some() || args.supervise.is_some() {
        let home = match server::home::Home::new(args.home.clone()) {
            Ok(h) => h,
            Err(e) => { eprintln!("ERROR: cannot prepare home directory: {e}"); std::process::exit(1); }
        };
        if args.mcp {
            let ctx = mcp::Ctx { home };
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            if let Err(e) = mcp::serve(stdin.lock(), stdout.lock(), mcp::tools::registry(), ctx) {
                eprintln!("ERROR: {e}");
                std::process::exit(1);
            }
        } else if let Some(tool) = &args.call {
            let ctx = mcp::Ctx { home };
            let ok = match mcp::run_call(tool, std::io::stdin().lock(), std::io::stdout().lock(), mcp::tools::registry(), ctx) {
                Ok(ok) => ok,
                Err(e) => { eprintln!("ERROR: {e}"); false }
            };
            std::process::exit(if ok { 0 } else { 1 });
        } else if let Some(id) = &args.job {
            if let Err(e) = server::jobs::run_job(&home, id) {
                eprintln!("ERROR: {e}");
                std::process::exit(1);
            }
            let ok = server::jobs::status(&home, id)
                .map(|s| s.state == server::jobs::JobState::Succeeded).unwrap_or(false);
            std::process::exit(if ok { 0 } else { 1 });
        } else if let Some(name) = &args.supervise {
            let name = match server::home::InstanceName::parse(name) {
                Ok(n) => n,
                Err(e) => { eprintln!("ERROR: {e}"); std::process::exit(1); }
            };
            if let Err(e) = server::supervise::supervise(&home, &name) {
                eprintln!("ERROR: {e}");
                std::process::exit(1);
            }
            std::process::exit(0);
        }
        return Ok(());
    }
    let default_port = if args.zookeeper { 2181 } else { 11211 };
    let port = args.port.unwrap_or(default_port);
    let timeout = time::Duration::from_millis(args.timeout);

    if args.zookeeper {
        let addr = format!("{}:{}", args.host, port);
        return zk::run_repl(&addr, timeout);
    }

    let h = helper::MyHelper::new();
    let mut rl: Editor<helper::MyHelper, DefaultHistory> = Editor::new()?;
    rl.set_helper(Some(h));
    #[cfg(feature = "with-file-history")]
    if rl.load_history("history.txt").is_err() {
        eprintln!("ERROR: No previous history.");
        std::process::exit(1);
    }

    let builder = Transport::builder()
        .rqid(args.req_id)
        .wait(timeout)
        .auth(args.sasl);

    let addr = if args.unix {
        args.host
    } else {
        format!("{}:{}", args.host, port)
    };

    let mut transport = if args.unix {
        builder.build_unix(addr)
    } else if args.udp {
        builder.build_udp(addr)
    } else {
        builder.build_tcp(addr)
    };

    /* The helper's validator keeps reading lines (typed, pasted or piped)
     * until every request has its data line(s), so each input is complete. */
    'repl: loop {
        let readline = rl.readline("");
        let continuing = rl.helper().is_some_and(|h| h.take_continuing());
        match readline {
            Ok(input) => {
                if !input.trim().is_empty() {
                    let _ = rl.add_history_entry(input.as_str());
                }
                for req in &request::split(&input).requests {
                    let quit = req.command().split_whitespace().next() == Some("quit");
                    if !(quit && args.udp) { transport.send(req); }
                    if quit { break 'repl; }
                }
            },
            /* Ctrl-C while data lines are awaited only cancels that input */
            Err(ReadlineError::Interrupted) if continuing => continue,
            Err(ReadlineError::Interrupted) => break,
            Err(ReadlineError::Eof) => break,
            Err(err) => { eprintln!("ERROR: {:?}", err); break }
        }
    }
    transport.finish();
    #[cfg(feature = "with-file-history")]
    let _ = rl.save_history("history.txt");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] fn repl_flags_still_parse() {
        let a = Args::try_parse_from(["arcus-cli", "--host", "h"]).unwrap();
        assert_eq!(a.host, "h");
        assert!(!a.mcp);
    }
    #[test] fn mcp_with_home_parses() {
        let a = Args::try_parse_from(["arcus-cli", "--mcp", "--home", "/x"]).unwrap();
        assert!(a.mcp);
        assert_eq!(a.home, Some(std::path::PathBuf::from("/x")));
    }
    #[test] fn mcp_conflicts_with_repl_flags() {
        assert!(Args::try_parse_from(["arcus-cli", "--mcp", "--zookeeper"]).is_err());
        assert!(Args::try_parse_from(["arcus-cli", "--mcp", "--host", "h"]).is_err());
    }
    #[test] fn call_with_home_parses() {
        let a = Args::try_parse_from(["arcus-cli", "--call", "zk_ls", "--home", "/x"]).unwrap();
        assert_eq!(a.call.as_deref(), Some("zk_ls"));
    }
    #[test] fn home_needs_a_mode() {
        assert!(Args::try_parse_from(["arcus-cli", "--home", "/x"]).is_err());
        assert!(Args::try_parse_from(["arcus-cli", "--job", "j1", "--home", "/x"]).is_ok());
        assert!(Args::try_parse_from(["arcus-cli", "--supervise", "n", "--home", "/x"]).is_ok());
    }
}
