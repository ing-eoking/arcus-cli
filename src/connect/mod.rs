pub mod tcp;
pub mod udp;
pub mod unix;

use std::net::ToSocketAddrs;
use self::tcp::TcpClient;
use self::udp::UdpClient;
use self::unix::UnixClient;

pub enum Transport {
    TCP,
    UDP,
    UNIX
}

pub struct Setting {
    pub addr: String,
    pub rqid: u16,
    pub time: u64,
    pub prot: Transport
}

pub struct Conn {
    transport: Transport,
    tcp: TcpClient,
    udp: UdpClient,
    unix: UnixClient
}

impl Conn {
    pub fn create() -> Self {
        Conn {
            transport: Transport::TCP,
            tcp: tcp::TcpClient::create(),
            udp: udp::UdpClient::create(),
            unix: unix::UnixClient::create()
        }
    }

    pub fn connect(&mut self, setting: Setting) {
        self.transport = setting.prot;
        match &self.transport {
            Transport::UNIX => self.unix.connect(setting.addr),
            net => {
                let mut addrs_iter = match setting.addr.to_socket_addrs() {
                    Ok(addr) => addr,
                    Err(err) => {
                        eprintln!("ERROR: {}", err);
                        std::process::exit(1);
                    }
                };
                match net {
                    Transport::TCP => self.tcp.connect(addrs_iter.next().unwrap()),
                    Transport::UDP => self.udp.connect(addrs_iter.next().unwrap(),
                                                       setting.rqid, setting.time),
                    _ => ()
                }
            }
        }
    }

    pub fn write(&mut self, line: String) {
        let mut buf = line;
        if buf.len() > 0 && &buf[buf.len() - 1..] != "\r" { buf.push('\r'); }
        buf.push('\n');
        match self.transport {
            Transport::TCP => self.tcp.write(buf),
            Transport::UDP => self.udp.write(buf),
            Transport::UNIX => self.unix.write(buf)
        }
    }
}
