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

fn main() {
    // 내 자격 증명 설정
    let creds = StaticCredentialProvider::new("alice", "wonderland", None);

    // 1. 서버로부터 메커니즘 리스트를 받음
    let server_mechs = get_server_mechanisms();

    // 2. 우선순위 기반으로 지원되는 메커니즘 선택
    let preferred_order = ["SCRAM-SHA-256", "SCRAM-SHA-1", "PLAIN"];

    let mech = preferred_order
        .iter()
        .find(|m| server_mechs.iter().any(|s| s == *m))
        .expect("No supported mechanism found");

    println!("Using mechanism: {}", mech);

    // 3. 세션 생성
    let mut session = ClientSession::new(mech, creds.clone())
        .expect("Failed to create SASL session");

    // 4. 서버에 보낼 초기 메시지 생성
    if let Some(msg) = session.initial_message() {
        println!("Initial message: {}", base64::encode(&msg));
        // 서버에 base64로 인코딩된 메시지를 보냅니다
    }

    // 5. 서버의 챌린지를 받았다고 가정하고 처리
    let server_challenge = b"..."; // <- 서버에서 받은 바이트들

    // (실제로는 네트워크 통해 받음)
    // let response = session.handle_challenge(&server_challenge).unwrap();
    // println!("Response to challenge: {:?}", base64::encode(&response));
}