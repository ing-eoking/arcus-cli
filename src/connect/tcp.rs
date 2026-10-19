use std::net::{TcpStream, Shutdown};
use std::io;
use super::internal::{Stream, StreamClient};

impl Stream for TcpStream {
    fn open(addr: &str) -> io::Result<Self> {
        let stream = TcpStream::connect(addr)?;
        stream.set_nodelay(true)?;
        Ok(stream)
    }
    fn try_clone(&self) -> io::Result<Self> { TcpStream::try_clone(self) }
    fn shutdown(&self, how: Shutdown) -> io::Result<()> { TcpStream::shutdown(self, how) }
}

pub type TcpClient = StreamClient<TcpStream>;
