//!A unidirectional pipe for bytes, analogous to a unix pipe. Available with the `io-pipe` feature toggle.

/// A unidirectional pipe for bytes, analogous to a unix pipe. Available with the `io-pipe` feature toggle.
#[cfg(feature = "io-pipe")]
pub mod pipe {
    use std::io;

    use bytes::{Buf, BufMut, BytesMut};

    /// The write-end of the pipe, receiving items to become available in the [`Reader`].
    ///
    /// It's commonly used with the [`std::io::Write`] trait it implements.
    pub struct Writer {
        /// The channel through which bytes are transferred. Useful for sending [`std::io::Error`]s instead.
        pub channel: std::sync::mpsc::SyncSender<io::Result<BytesMut>>,
        buf: BytesMut,
    }

    /// The read-end of the pipe, implementing the [`std::io::Read`] trait.
    pub struct Reader {
        channel: std::sync::mpsc::Receiver<io::Result<BytesMut>>,
        buf: BytesMut,
        interrupt: Option<Interrupt>,
    }

    /// A flag that is checked while waiting for data, and the error to fail with once it is set.
    type Interrupt = (std::sync::Arc<std::sync::atomic::AtomicBool>, fn() -> io::Error);

    impl Reader {
        /// Make reads that wait for data fail with the error that `error` creates as soon as `flag` is set, which it is
        /// checked for every 25 ms of waiting, and keep failing.
        ///
        /// This lets another thread end a read that blocks on a writer that doesn't write.
        pub fn fail_when_set(
            mut self,
            flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
            error: fn() -> io::Error,
        ) -> Self {
            self.interrupt = Some((flag, error));
            self
        }

        /// Wait for the next item, or return `None` if the writer is gone.
        fn next_item(&self) -> Option<io::Result<BytesMut>> {
            let Some((flag, error)) = &self.interrupt else {
                return self.channel.recv().ok();
            };
            loop {
                if flag.load(std::sync::atomic::Ordering::SeqCst) {
                    return Some(Err(error()));
                }
                match self.channel.recv_timeout(std::time::Duration::from_millis(25)) {
                    Ok(item) => return Some(item),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return None,
                }
            }
        }
    }

    impl io::BufRead for Reader {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            if self.buf.is_empty() {
                match self.next_item() {
                    Some(Ok(buf)) => self.buf = buf,
                    Some(Err(err)) => return Err(err),
                    None => {}
                }
            }
            Ok(&self.buf)
        }

        fn consume(&mut self, amt: usize) {
            self.buf.advance(amt.min(self.buf.len()));
        }
    }

    impl io::Read for Reader {
        fn read(&mut self, mut out: &mut [u8]) -> io::Result<usize> {
            let mut written = 0;
            while !out.is_empty() {
                if self.buf.is_empty() {
                    match self.next_item() {
                        Some(Ok(buf)) => self.buf = buf,
                        Some(Err(err)) => return Err(err),
                        None => break,
                    }
                }
                let bytes_to_write = self.buf.len().min(out.len());
                let (to_write, rest) = out.split_at_mut(bytes_to_write);
                self.buf.split_to(bytes_to_write).copy_to_slice(to_write);
                out = rest;
                written += bytes_to_write;
            }
            Ok(written)
        }
    }

    impl io::Write for Writer {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.buf.put_slice(buf);
            self.channel
                .send(Ok(self.buf.split()))
                .map_err(|err| io::Error::new(io::ErrorKind::BrokenPipe, err))?;
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Returns the _([`write`][Writer], [`read`][Reader])_ ends of a pipe for transferring bytes, analogous to a unix pipe.
    ///
    /// * `in_flight_writes` defines the amount of chunks of bytes to keep in memory until the `write` end will block when writing.
    ///   If `0`, the `write` end will always block until the `read` end consumes the transferred bytes.
    pub fn unidirectional(in_flight_writes: usize) -> (Writer, Reader) {
        let (tx, rx) = std::sync::mpsc::sync_channel(in_flight_writes);
        (
            Writer {
                channel: tx,
                buf: BytesMut::with_capacity(4096),
            },
            Reader {
                channel: rx,
                buf: BytesMut::new(),
                interrupt: None,
            },
        )
    }
}
