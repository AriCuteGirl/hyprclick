//! Unix sockets don't exist on Windows. Only Hyprland and the Linux pad
//! service use them, and both already cope with a connection that fails, so
//! these stand-ins just fail.

use std::io;
use std::path::Path;
use std::time::Duration;

fn unsupported() -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, "not available on Windows")
}

pub struct UnixStream;

impl UnixStream {
    pub fn connect<P: AsRef<Path>>(_: P) -> io::Result<Self> {
        Err(unsupported())
    }

    pub fn set_read_timeout(&self, _: Option<Duration>) -> io::Result<()> {
        Err(unsupported())
    }
}

impl io::Read for UnixStream {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(unsupported())
    }
}

impl io::Read for &UnixStream {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(unsupported())
    }
}

impl io::Write for UnixStream {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(unsupported())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl io::Write for &UnixStream {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(unsupported())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
