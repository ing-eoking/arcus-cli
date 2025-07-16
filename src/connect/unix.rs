use std::thread;
use std::io::{self, BufReader, ErrorKind};
use std::io::prelude::*;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::thread::JoinHandle;
use rsasl::prelude::*;

#[derive(Default)]
pub struct UnixClient {
    pub auth: bool,
    conn: Option<UnixStream>,
    hand: Option<JoinHandle<()>>,
    runn: bool,
}

impl UnixClient {
    pub fn connect(&mut self, address: &str) {
        if !self.conn.is_none() { drop(self.conn.take().unwrap()); }
        self.runn = false;
        match UnixStream::connect(address) {
            Ok(sock) => {
                self.conn = Some(sock.try_clone().unwrap());
                if self.auth {
                    self.authenticate(sock.try_clone().unwrap());
                }
                self.hand = self.activate_reader(sock);
                self.runn = true;
            },
            Err(err) => {
                eprintln!("ERROR: {}", err);
                if err.kind() != ErrorKind::ConnectionRefused {
                    std::process::exit(1);
                }
            }
        };
    }

    pub fn write(&mut self, line: String) -> bool {
        return match self.conn.as_mut() {
            None => true,
            Some(conn) => matches!(
                conn.write(line.as_bytes()),
                Err(ref err) if err.kind() == ErrorKind::BrokenPipe
            ),
        };
    }

    fn read_line(&mut self, rbuf: &mut BufReader<UnixStream>) -> String {
        let mut line = String::new();
        match rbuf.read_line(&mut line) {
            Err(e) => eprintln!("ERROR: {}", e),
            _ => ()
        }
        return line;
    }

    fn activate_reader(&mut self, sock: UnixStream) -> Option<JoinHandle<()>> {
        let mut rbuf = BufReader::new(sock);
        let mut line = String::new();
        return Some(thread::spawn(move || {
            loop {
                match rbuf.read_line(&mut line) {
                    Err(err) => eprintln!("ERROR: {}", err),
                    Ok(0) => break,
                    _ => ()
                }
                print!("{}", line);
                line.clear();
            }
        }));
    }

    fn authenticate(&mut self, conn: UnixStream) {
        let mut rbuf = BufReader::new(conn);
        let mut username = String::new();
        print!("username: ");
        io::stdout().flush().unwrap();
        io::stdin().read_line(&mut username).unwrap();
        let username = username.trim().to_string();
        let password = rpassword::prompt_password("password: ").unwrap();

        let config = SASLConfig::with_credentials(None, username, password).unwrap();
        let client = SASLClient::new(config);

        self.write("sasl mech\r\n".to_string());
        let mut line = self.read_line(&mut rbuf);

        if !line.starts_with("SASL_MECH ") {
            eprintln!("AUTH_ERROR: Protocol error");
            return;
        }

        let mech_list: &str = &line["SASL_MECH ".len()..line.len() - "\r\n".len()];
        let server_mech: Vec<&Mechname> = mech_list.split_whitespace()
                                                   .filter_map(|s| Mechname::parse(s.as_bytes()).ok())
                                                   .collect();
        let mut session = match client.start_suggested(&server_mech) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("AUTH_ERROR: {}", e);
                return;
            }
        };

        let mut mech = Some(session.get_mechname().to_string() + " ");
        let mut resp: Option<Vec<u8>> = None;
        loop {
            let mut out = Vec::new();
            let state = match session.step(resp.as_deref(), &mut out) {
                Ok(res) => res,
                Err(e) => {
                    eprintln!("AUTH_ERROR: {}", e);
                    break;
                }
            };

            if !state.is_running() {
                if state.is_finished() {
                } else {
                    eprintln!("AUTH_ERROR: SASL not finished");
                    break;
                }
            }

            let out_str = match std::str::from_utf8(&out) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("ERROR: {}", e);
                    break;
                }
            };

            let req = format!("sasl auth {}{}\r\n{}\r\n", mech.clone().unwrap_or("".to_string()),
                                                                   out_str.len(), out_str);
            self.write(req);
            match self.read_line(&mut rbuf).as_str() {
                s if s.starts_with("SASL_CONTINUE") => {
                    line = self.read_line(&mut rbuf);
                    resp = Some(line.strip_suffix("\r\n").unwrap_or(&line).as_bytes().to_vec());
                    mech = None;
                },
                "SASL_OK\r\n" => break,
                _ => {
                    eprintln!("AUTH_ERROR: SASL authenticate failed");
                    break;
                }
            }
        }
    }
}

impl Drop for UnixClient {
    fn drop(&mut self) {
        if self.runn {
            self.conn.as_mut().unwrap().shutdown(Shutdown::Write).unwrap();
            self.hand.take().unwrap().join().unwrap();
        }
    }
}
