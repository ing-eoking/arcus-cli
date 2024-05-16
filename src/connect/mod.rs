pub mod tcp;
pub mod udp;
pub mod unix;

use self::tcp::TcpClient;
use self::udp::UdpClient;
use self::unix::UnixClient;

pub enum Transport {
    TCP(String, TcpClient),
    UDP(String, UdpClient),
    UNIX(String, UnixClient)
}

impl Transport {
    pub fn setting(&mut self, rqid: u16, time: u64, _auth: bool) {
        match self {
            Transport::TCP(_, clnt) => {
                clnt.auth = _auth;
            }
            Transport::UDP(_, clnt) => {
                clnt.rqid = rqid;
                clnt.time = time;
                clnt.auth = _auth;
            },
            Transport::UNIX(_, clnt) => {
                clnt.auth = _auth;
            }
        }
    }

    pub fn write(&mut self, mut line: String) {
        if !line.is_empty() && !line.ends_with('\r') { line.push('\r'); }
        line.push('\n');
        match self {
            Transport::TCP(addr, clnt) =>
                if clnt.write(line) { clnt.connect(addr) },
            Transport::UDP(addr, clnt) =>
                if clnt.write(line) { clnt.connect(addr) },
            Transport::UNIX(addr, clnt) =>
                if clnt.write(line) { clnt.connect(addr) },
        }
    }
}
