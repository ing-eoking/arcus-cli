mod helper;
mod connect;
mod request;
mod zk;

use std::time;
use rustyline::history::DefaultHistory;
use rustyline::Editor;
use rustyline::error::ReadlineError;
use clap::{ArgAction, Parser};
use connect::Transport;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
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
}

fn main() -> rustyline::Result<()> {
    let args = Args::parse();
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
