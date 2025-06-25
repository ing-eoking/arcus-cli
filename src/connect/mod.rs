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
    pub fn setting(&mut self, rqid: u16, time: u64) {
        match self {
            Transport::UDP(_, clnt) => {
                clnt.rqid = rqid;
                clnt.time = time;
            },
            _ => ()
        }
    }
    pub fn write(&mut self, line: String) {
        let mut buf = line;
        if buf.len() > 0 && &buf[buf.len() - 1..] != "\r" { buf.push('\r'); }
        buf.push('\n');
        match self {
            Transport::TCP(addr, clnt) =>
                if clnt.write(buf) { clnt.connect(addr) },
            Transport::UDP(addr, clnt) =>
                if clnt.write(buf) { clnt.connect(addr) },
            Transport::UNIX(addr, clnt) =>
                if clnt.write(buf) { clnt.connect(addr) },
        }
    }
}
