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
    /// Host name or IP
    #[arg(long, default_value_t = String::from("localhost"))]
    host: String,

    /// Port Number
    #[arg(short, long, default_value_t = 11211)]
    port: u16,

    /// Use UDP protocol
    #[clap(long, short, action=ArgAction::SetTrue)]
    udp: bool,

    /// Request ID for UDP
    #[arg(long, default_value_t = 1)]
    req_id: u16,

    /// Unix socket path (disables network support)
    #[arg(long, default_value_t = String::from(""))]
    unix_path: String,

    /// Timeout(ms)
    #[arg(short, long, default_value_t = 300)]
    timeout: u64

}

fn main() -> rustyline::Result<()> {
    let args = Args::parse();
    let timeout = time::Duration::from_millis(args.timeout);
    let h = helper::MyHelper::new();
    let mut rl: Editor<helper::MyHelper, DefaultHistory> = Editor::new()?;
    rl.set_helper(Some(h));
    #[cfg(feature = "with-file-history")]
    if rl.load_history("history.txt").is_err() {
        eprintln!("ERROR: No previous history.");
        std::process::exit(1);
    }
    let transport = if args.unix_path.len() > 0 { connect::Transport::UNIX }
                               else if args.udp { connect::Transport::UDP }
                               else { connect::Transport::TCP };
    let mut conn = connect::Conn::create();
    conn.connect(args.host, args.port,
                 args.req_id, args.timeout, transport);
    loop {
        let readline = rl.readline("");
        match readline {
            Ok(line) => {
                let _ = rl.add_history_entry(line.as_str());
                if line == "quit" { break }
                conn.write(line);
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
