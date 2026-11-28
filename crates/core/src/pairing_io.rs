//! Bounded, cancellable I/O shared by invitation and comparison pairing.
//!
//! Retry short socket deadlines inside Read/Write so their caller's read_exact/write_all retains partial frames.
//! Windows may not wake an already blocked receive when a duplicated socket is shut down.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// A pairing socket whose short I/O slices observe cancellation and a total exchange deadline.
pub struct CancellableIo<'a, F> {
    stream: &'a mut TcpStream,
    cancelled: F,
    until: Instant,
}

impl<'a, F: Fn() -> bool> CancellableIo<'a, F> {
    /// Sets short socket timeouts; all pairing I/O on this socket should use the wrapper.
    pub fn new(stream: &'a mut TcpStream, until: Instant, cancelled: F) -> io::Result<Self> {
        let slice = Some(Duration::from_millis(100));
        stream.set_read_timeout(slice)?;
        stream.set_write_timeout(slice)?;
        Ok(Self { stream, cancelled, until })
    }

    pub(crate) fn check(&self) -> io::Result<()> {
        if (self.cancelled)() {
            return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "Pairing stopped."));
        }
        if Instant::now() >= self.until {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "The other screen did not finish pairing in time."));
        }
        Ok(())
    }
}

impl<F: Fn() -> bool> Read for CancellableIo<'_, F> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            self.check()?;
            match self.stream.read(bytes) {
                Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
                result => return result,
            }
        }
    }
}

impl<F: Fn() -> bool> Write for CancellableIo<'_, F> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        loop {
            self.check()?;
            match self.stream.write(bytes) {
                Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
                result => return result,
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.check()?;
        self.stream.flush()
    }
}
