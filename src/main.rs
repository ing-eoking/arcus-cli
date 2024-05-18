use async_std;
use std::io::Write;
use clap::{Parser, ArgAction};
use rustyline_async::{Readline, ReadlineError, ReadlineEvent};
use futures_util::{select, FutureExt};

mod connect;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Host name or IP
    #[arg(long, default_value_t = String::from("localhost"))]
    host: String,

    /// Port Number
    #[arg(short, long, default_value_t = 11211)]
    port: u32,

    /// Use UDP protocol
    #[clap(long, short, action=ArgAction::SetTrue)]
    udp: bool,

    /// Request ID for UDP
    #[arg(long, default_value_t = 1)]
    req_id: i32,

    /// Unix socket path (disables network support)
    #[arg(long, default_value_t = String::from(""))]
    unix_path: String,

}

#[async_std::main]
async fn main() -> Result<(), ReadlineError> {
    let args = Args::parse();
    let (mut rl, mut stdout) = Readline::new("".to_owned()).unwrap();

    let transport = if args.unix_path.len() > 0 { connect::Transport::UNIX }
                               else if args.udp { connect::Transport::UDP }
                               else { connect::Transport::TCP };
    let mut conn = connect::Conn::create();
    conn.connect(args.host, args.port, transport);
    loop {
        select! {
            command = rl.readline().fuse() => match command {
                Ok(ReadlineEvent::Line(line)) => {
                    rl.add_history_entry(line.to_owned());
                    if line == "quit" { break }
                    writeln!(stdout, "{}", line)?;
                },
                Ok(ReadlineEvent::Eof) => break,
                Ok(ReadlineEvent::Interrupted) => break,
                Err(err) => { writeln!(stdout, "ERROR: {}", err)?; break }
            }
        }

        //let readline = rl.readline("");
        // match readline {
        //     Ok(line) => {
        //         let _ = rl.add_history_entry(line.as_str());
        //         if line == "quit" { break }
        //         conn.write(line);
        //     },
        //     Err(ReadlineError::Interrupted) => { break },
        //     Err(ReadlineError::Eof) => { break },
        //     Err(err) => { eprintln!("ERROR: {:?}", err); break }
        // }
    }
    Ok(())
}
