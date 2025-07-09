use sasl::client::mechanisms;
use sasl::common::Credentials;
use std::io::{self, Write};

pub fn authenticate() {
    let mut username = String::new();

    print!("username: ");
    io::stdout().flush().unwrap();
    io::stdin().read_line(&mut username).unwrap();
    let username = username.trim();
    let password = rpassword::prompt_password("password: ").unwrap();

    let creds = Credentials::default()
                                         .with_username(username)
                                         .with_password(password);
}
use rsasl::prelude::*;
use rsasl::mechanisms::Mechname;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 서버에서 받은 메커니즘 리스트 (예: "PLAIN SCRAM-SHA-256")
    let mech_str = "PLAIN SCRAM-SHA-256";
    let offered: Vec<Mechname> = mech_str
        .split_whitespace()
        .filter_map(|s| Mechname::parse(s.as_bytes()).ok())
        .collect();

    let config = Arc::new(
        SASLConfig::builder()
            .user("alice")
            .password("secret123")
            .build()
    );

    let client = SASLClient::new(config);
    let mut session = client
        .start_suggested(&offered)
        .expect("서버와 공유 가능한 메커니즘 없음");

    println!("선택된 메커니즘: {}", session.get_mechname());

    let mut outgoing: Option<Vec<u8>> = None;

    // StreamOverChannel 없이 step loop 예제
    loop {
        let state = if outgoing.is_some() {
            session.step(outgoing.as_deref(), &mut std::io::sink())?
        } else {
            session.step(outgoing.as_deref(), &mut std::io::sink())?
        };

        if !state.is_running() {
            println!("인증 완료 ({}", state.is_finish_success());
            break;
        }

        outgoing = Some(session.get_last_msg().into());
        println!("클라이언트 메시지 → 서버: {:?}", outgoing.as_ref().unwrap());
        // 실제론 서버 응답을 네트워크로부터 읽어와야 함
        // 예: outgoing = Some(received_bytes);
    }

    Ok(())
}