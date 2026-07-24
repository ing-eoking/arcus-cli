mod helper;
mod connect;
mod zk;

use std::{thread, time};
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

    /// Timeout(μs)
    #[arg(short, long, default_value_t = 100)]
    timeout: u64,

    /// Authenticate with sasl
    #[clap(long, action=ArgAction::SetTrue)]
    sasl: bool,
}

fn main() -> rustyline::Result<()> {
    let args = Args::parse();
    let default_port = if args.zookeeper { 2181 } else { 11211 };
    let port = args.port.unwrap_or(default_port);
    let timeout = time::Duration::from_micros(args.timeout);

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
        .time(args.timeout)
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

    loop {
        let readline = rl.readline("");
        match readline {
            Ok(line) => {
                let _ = rl.add_history_entry(line.as_str());
                if line == "quit" { break }
                transport.write(line);
            },
            Err(ReadlineError::Interrupted) => { break },
            Err(ReadlineError::Eof) => { thread::sleep(timeout); break },
            Err(err) => { eprintln!("ERROR: {:?}", err); break }
        }
    }
    #[cfg(feature = "with-file-history")]
    rl.save_history("history.txt");
    Ok(())
}

