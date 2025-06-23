pub mod tcp;
pub mod udp;
pub mod unix;

use self::tcp::TcpClient;
use self::udp::UdpClient;
use self::unix::UnixClient;

#[derive(Default)]
pub enum Transport {
    #[default]
    NONE,
    TCP(String, TcpClient),
    UDP(String, UdpClient),
    UNIX(String, UnixClient)
}


#[derive(Default)]
pub struct Conn {
    transport: Transport,
}

impl Conn {
    pub fn connect(&mut self, transport: Transport, rqid: u16, time: u64) {
        self.transport = transport;

        match &mut self.transport {
            Transport::TCP(addr, clnt) => {
                clnt.connect(addr);
            },
            Transport::UDP(addr, clnt) => {
                clnt.rqid = rqid;
                clnt.time = time;
                clnt.connect(addr);
            },
            Transport::UNIX(addr, clnt) => {
                clnt.connect(addr);
            },
            _ => {}
        }
    }

    pub fn write(&mut self, line: String) {
        let mut buf = line;
        if buf.len() > 0 && &buf[buf.len() - 1..] != "\r" { buf.push('\r'); }
        buf.push('\n');
        match &mut self.transport {
            Transport::TCP(_, clnt) => clnt.write(buf),
            Transport::UDP(_, clnt) => clnt.write(buf),
            Transport::UNIX(_, clnt) => clnt.write(buf),
            _ => ()
        }
    }
}
