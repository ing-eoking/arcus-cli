use std::os::unix::net::UnixStream;
use std::net::Shutdown;
use std::io;
use super::internal::{Stream, StreamClient};

impl Stream for UnixStream {
    fn open(path: &str) -> io::Result<Self> { UnixStream::connect(path) }
    fn try_clone(&self) -> io::Result<Self> { UnixStream::try_clone(self) }
    fn shutdown(&self, how: Shutdown) -> io::Result<()> { UnixStream::shutdown(self, how) }
}

pub type UnixClient = StreamClient<UnixStream>;
