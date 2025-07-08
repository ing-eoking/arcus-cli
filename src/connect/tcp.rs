use std::thread;
use std::io::{self, Write, BufReader, ErrorKind};
use std::io::prelude::*;
use std::net::{SocketAddr, TcpStream, Shutdown, ToSocketAddrs};
use std::thread::JoinHandle;
use rsasl::prelude::*;

use std::sync::Arc;

#[derive(Default)]
pub struct TcpClient {
    pub auth: bool,
    addr: Option<SocketAddr>,
    conn: Option<TcpStream>,
    hand: Option<JoinHandle<()>>,
    runn: bool,
}

impl TcpClient {
    pub fn connect(&mut self, address: &str) {
        if !self.conn.is_none() { /* TODO */
            drop(self.conn.take().unwrap());
            self.addr = None;
        }
        self.runn = false;
        let addrs_iter = if self.addr.is_none() {
            match address.to_socket_addrs() {
                Ok(addrs) => addrs.collect::<Vec<_>>(),
                Err(err) => {
                    eprintln!("ERROR: {}", err);
                    std::process::exit(1);
                }
            }
        } else {
            vec![self.addr.unwrap()]
        };

        let mut last_err = None;
        for addr in addrs_iter {
            match TcpStream::connect(addr) {
                Ok(stream) => {
                    self.conn = Some(stream.try_clone().unwrap());
                    self.addr = Some(addr); /* No need */
                    if self.auth {
                        self.authenticate();
                    }

                    self.hand = self.activate_reader(stream);
                    self.runn = true;
                    return;
                },
                Err(e) => last_err = Some(e)
            }
        };

        let err = last_err.unwrap();
        eprintln!("ERROR: {}", err);
        if err.kind() != ErrorKind::ConnectionRefused {
            std::process::exit(1);
        }
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

    pub fn read(&mut self) -> String {
        let mut line = String::new();
        match self.conn.as_mut() {
            None => eprintln!("ERROR: No connection"),
            Some(conn) => {
                let mut rbuf = BufReader::new(conn);
                match rbuf.read_line(&mut line) {
                    Err(e) => eprintln!("ERROR: {}", e),
                    _ => ()
                }
            }
        }
        return line;
    }

    fn authenticate(&mut self) {
        let mut username = String::new();

        print!("username: ");
        io::stdout().flush().unwrap();
        io::stdin().read_line(&mut username).unwrap();
        let username = username.trim();
        let password = rpassword::prompt_password("password: ").unwrap();

        let config = SASLConfig::with_credentials(None, username.to_string(), password).unwrap();

        let sasl = SASLClient::new(config.clone());


        // let creds = Credentials::default()
        //                         .with_username(username)
        //                         .with_password(password);

        self.write("sasl mech\r\n".to_string());
        let line = self.read();

        if !line.starts_with("SASL_MECH ") {
            eprintln!("ERROR: SASL_MECH error");
        }

        let mech = &line["SASL_MECH ".len()..line.len() - "\r\n".len()];
        let server_mechs: Vec<&str> = mech.strip_prefix("SASL_MECH ")
                                          .unwrap_or("")
                                          .trim_end_matches("\r\n")
                                          .split_whitespace()
                                          .collect();


        println!("<RES {:?}", mech);

    }

    fn activate_reader(&mut self, sock: TcpStream) -> Option<JoinHandle<()>> {
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
}

impl Drop for TcpClient {
    fn drop(&mut self) {
        if self.runn {
            self.conn.as_mut().unwrap().shutdown(Shutdown::Write).unwrap();
            self.hand.take().unwrap().join().unwrap();
        }
    }
}
