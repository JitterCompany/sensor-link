//! Bridges the UART halves (embedded-io-async 0.6) to the single
//! `BufRead + Write` object (embedded-io-async 0.7) that
//! `embassy_net_ppp::Runner::run` consumes.

use core::fmt::Debug;

/// Suggested fill-buffer size: one DMA-ring drain per `fill_buf`.
pub const FILL_BUF_LEN: usize = 512;

#[derive(Debug)]
pub enum PppIoError<RE, WE> {
    Read(RE),
    Write(WE),
}

impl<RE: Debug, WE: Debug> core::fmt::Display for PppIoError<RE, WE> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PppIoError::Read(e) => write!(f, "ppp uart read: {e:?}"),
            PppIoError::Write(e) => write!(f, "ppp uart write: {e:?}"),
        }
    }
}

impl<RE: Debug, WE: Debug> core::error::Error for PppIoError<RE, WE> {}

impl<RE: Debug, WE: Debug> embedded_io_async_07::Error for PppIoError<RE, WE> {
    fn kind(&self) -> embedded_io_async_07::ErrorKind {
        embedded_io_async_07::ErrorKind::Other
    }
}

pub struct PppIo<'a, R, W> {
    rx: R,
    tx: W,
    buf: &'a mut [u8],
    /// Consumed prefix of the buffered bytes.
    pos: usize,
    /// Valid bytes in `buf`.
    cap: usize,
}

impl<'a, R, W> PppIo<'a, R, W>
where
    R: embedded_io_async::Read,
    W: embedded_io_async::Write,
{
    pub fn new(rx: R, tx: W, buf: &'a mut [u8]) -> Self {
        Self {
            rx,
            tx,
            buf,
            pos: 0,
            cap: 0,
        }
    }

    /// Releases the UART halves (for returning to AT command mode).
    pub fn release(self) -> (R, W) {
        (self.rx, self.tx)
    }
}

impl<R, W> embedded_io_async_07::ErrorType for PppIo<'_, R, W>
where
    R: embedded_io_async::Read,
    W: embedded_io_async::Write,
{
    type Error = PppIoError<R::Error, W::Error>;
}

impl<R, W> embedded_io_async_07::Read for PppIo<'_, R, W>
where
    R: embedded_io_async::Read,
    W: embedded_io_async::Write,
{
    async fn read(&mut self, out: &mut [u8]) -> Result<usize, Self::Error> {
        use embedded_io_async_07::BufRead;
        let available = self.fill_buf().await?;
        let n = available.len().min(out.len());
        out[..n].copy_from_slice(&available[..n]);
        self.consume(n);
        Ok(n)
    }
}

impl<R, W> embedded_io_async_07::BufRead for PppIo<'_, R, W>
where
    R: embedded_io_async::Read,
    W: embedded_io_async::Write,
{
    async fn fill_buf(&mut self) -> Result<&[u8], Self::Error> {
        if self.pos == self.cap {
            self.pos = 0;
            self.cap = self
                .rx
                .read(self.buf)
                .await
                .map_err(PppIoError::Read)?;
        }
        Ok(&self.buf[self.pos..self.cap])
    }

    fn consume(&mut self, amt: usize) {
        self.pos = (self.pos + amt).min(self.cap);
    }
}

impl<R, W> embedded_io_async_07::Write for PppIo<'_, R, W>
where
    R: embedded_io_async::Read,
    W: embedded_io_async::Write,
{
    async fn write(&mut self, data: &[u8]) -> Result<usize, Self::Error> {
        self.tx.write(data).await.map_err(PppIoError::Write)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.tx.flush().await.map_err(PppIoError::Write)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_io_async_07::{BufRead, Write};

    /// 0.6 reader yielding scripted chunks, then EOF.
    struct ScriptedRx {
        chunks: std::vec::Vec<std::vec::Vec<u8>>,
    }

    impl embedded_io_async::ErrorType for ScriptedRx {
        type Error = core::convert::Infallible;
    }
    impl embedded_io_async::Read for ScriptedRx {
        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
            if self.chunks.is_empty() {
                return Ok(0);
            }
            let chunk = self.chunks.remove(0);
            let n = chunk.len().min(buf.len());
            buf[..n].copy_from_slice(&chunk[..n]);
            // A chunk larger than the caller's buffer would be lost; the
            // scripted chunks are chosen smaller than the fill buffer.
            assert_eq!(n, chunk.len(), "test chunk larger than read buffer");
            Ok(n)
        }
    }

    #[derive(Default)]
    struct RecordingTx {
        written: std::vec::Vec<u8>,
        flushes: usize,
    }

    impl embedded_io_async::ErrorType for RecordingTx {
        type Error = core::convert::Infallible;
    }
    impl embedded_io_async::Write for RecordingTx {
        async fn write(&mut self, data: &[u8]) -> Result<usize, Self::Error> {
            // Short write of at most 4 bytes per call, like a small DMA scratch.
            let n = data.len().min(4);
            self.written.extend_from_slice(&data[..n]);
            Ok(n)
        }
        async fn flush(&mut self) -> Result<(), Self::Error> {
            self.flushes += 1;
            Ok(())
        }
    }

    #[tokio::test]
    async fn buffers_and_consumes_across_chunks() {
        let rx = ScriptedRx {
            chunks: vec![b"hello ".to_vec(), b"world".to_vec()],
        };
        let mut buf = [0u8; 8];
        let mut io = PppIo::new(rx, RecordingTx::default(), &mut buf);

        let first = io.fill_buf().await.unwrap();
        assert_eq!(first, b"hello ");
        // Partial consume: the rest must remain visible.
        io.consume(2);
        assert_eq!(io.fill_buf().await.unwrap(), b"llo ");
        io.consume(4);

        assert_eq!(io.fill_buf().await.unwrap(), b"world");
        io.consume(5);

        // EOF: empty slice.
        assert_eq!(io.fill_buf().await.unwrap(), b"");
    }

    #[tokio::test]
    async fn write_passthrough_reports_short_writes() {
        let rx = ScriptedRx { chunks: vec![] };
        let mut buf = [0u8; 8];
        let mut io = PppIo::new(rx, RecordingTx::default(), &mut buf);

        assert_eq!(io.write(b"abcdef").await.unwrap(), 4);
        assert_eq!(io.write(b"ef").await.unwrap(), 2);
        io.flush().await.unwrap();

        let (_, tx) = io.release();
        assert_eq!(tx.written, b"abcdef");
        assert_eq!(tx.flushes, 1);
    }
}
