use std::io::{self, BufRead, BufReader, Read, Write};
use rsasl::prelude::*;

pub(crate) fn authenticate<S>(mut stream: S) -> io::Result<S>
where S: Read + Write
{
    let mut rbuf = BufReader::new(&mut stream);

    let mut username = String::new();
    print!("username: ");
    io::stdout().flush()?;
    io::stdin().read_line(&mut username)?;
    let username = username.trim().to_string();
    let password = rpassword::prompt_password("password: ").unwrap();

    let config = SASLConfig::with_credentials(
        None,
        username,
        password
    ).map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Config Error: {}", e)))?;

    let client = SASLClient::new(config);

    rbuf.get_mut().write_all(b"sasl mech\r\n")?;
    rbuf.get_mut().flush()?;

    let mut line = String::new();
    rbuf.read_line(&mut line)?;

    if !line.starts_with("SASL_MECH ") {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Protocol error: No SASL_MECH"));
    }

    let mech_list = &line["SASL_MECH ".len()..line.len() - "\r\n".len()];
    let server_mechs: Vec<&Mechname> = mech_list.split_whitespace()
                                                .filter_map(|s| Mechname::parse(s.as_bytes()).ok())
                                                .collect();

    let mut session = client.start_suggested(&server_mechs)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Negotiation failed: {}", e)))?;

    let mut mech_param = Some(session.get_mechname().to_string() + " ");
    let mut input_data: Option<Vec<u8>> = None;

    loop {
        let mut out = Vec::new();
        let state = session.step(input_data.as_deref(), &mut out)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

        let out_str = String::from_utf8(out).unwrap_or_default();

        let req = format!("sasl auth {}{}\r\n{}\r\n",
            mech_param.take().unwrap_or_default(),
            out_str.len(),
            out_str
        );

        rbuf.get_mut().write_all(req.as_bytes())?;
        rbuf.get_mut().flush()?;

        line.clear();
        rbuf.read_line(&mut line)?;

        if line.starts_with("SASL_CONTINUE") {
            line.clear();
            rbuf.read_line(&mut line)?;
            input_data = Some(line.trim_end().as_bytes().to_vec());
        } else if line == "SASL_OK\r\n" {
            break;
        } else {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("Auth failed: {}", line.trim())));
        }

        if state.is_finished() {
            /* dummy check */
        }
    }

    Ok(stream)
}

pub(crate) fn spawn_reader_loop<S>(stream: S) -> std::thread::JoinHandle<()>
where
    S: Read + Send + 'static
{
    std::thread::spawn(move || {
        let mut rbuf = BufReader::new(stream);
        let mut line = String::new();
        loop {
            match rbuf.read_line(&mut line) {
                Ok(0) => break, // EOF
                Ok(_) => {
                    print!("{}", line);
                    line.clear();
                }
                Err(_) => break,
            }
        }
    })
}