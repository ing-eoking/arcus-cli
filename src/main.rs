mod helper;
mod connect;

use std::{thread, time};
use rustyline::history::DefaultHistory;
use rustyline::Editor;
use rustyline::error::ReadlineError;
use clap::{ArgAction, Parser};

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Host name or IP or Unix path
    #[arg(long, default_value_t = String::from("127.0.0.1"))]
    host: String,

    /// Port Number
    #[arg(short, long, default_value_t = 11211)]
    port: u16,

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

    /// Use Sasl
    #[clap(long, action=ArgAction::SetTrue)]
    sasl: bool,
}

fn main() -> rustyline::Result<()> {
    let args = Args::parse();
    let timeout = time::Duration::from_micros(args.timeout);
    let h = helper::MyHelper::new();
    let mut rl: Editor<helper::MyHelper, DefaultHistory> = Editor::new()?;
    rl.set_helper(Some(h));
    #[cfg(feature = "with-file-history")]
    if rl.load_history("history.txt").is_err() {
        eprintln!("ERROR: No previous history.");
        std::process::exit(1);
    }

    let mut transport = if args.unix {
        connect::Transport::UNIX(args.host, Default::default())
    } else if args.udp {
        connect::Transport::UDP(format!("{}:{}", args.host, args.port), Default::default())
    } else {
        connect::Transport::TCP(format!("{}:{}", args.host, args.port), Default::default())
    };

    transport.setting(args.req_id, args.timeout, args.sasl);
    transport.write("".to_string());
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

