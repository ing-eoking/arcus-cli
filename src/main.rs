use clap::Parser;
use rustyline::error::ReadlineError;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Host name or IP
    #[arg(long, default_value_t = String::from("127.0.0.1"))]
    host: String,

    /// Port Number
    #[arg(short, long, default_value_t = 11211)]
    port: u32,

    /// Enable UDP
    #[clap(long, short, action=ArgAction::SetFalse)]
    udp: bool,
}

fn main() -> rustyline::Result<()> {
    let args = Args::parse();
    let mut rl = rustyline::DefaultEditor::new()?;
    #[cfg(feature = "with-file-history")]
    if rl.load_history("history.txt").is_err() {
        println!("CLI_ERROR: No previous history.");
    }
    loop {
        let readline = rl.readline("");
        match readline {
            Ok(line) => {
                let _ = rl.add_history_entry(line.as_str());
                if line == "quit" { break }
                println!("{}:{} {} > {}", args.host, args.port, args.udp, line);
            },
            Err(ReadlineError::Interrupted) => { break },
            Err(ReadlineError::Eof) => { break },
            Err(err) => { println!("CLI_ERROR: {:?}", err); break }
        }
    }
    #[cfg(feature = "with-file-history")]
    rl.save_history("history.txt");
    Ok(())
}
