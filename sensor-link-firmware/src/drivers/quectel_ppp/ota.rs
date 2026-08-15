//! Streaming firmware download: minimal HTTP/1.1 GET over the own stack.
//!
//! The update flow calls `download_file(url)` once and then pulls the
//! (base64-encoded) artifact in 400-byte chunks via `read_file_chunk`; the
//! modem driver used to buffer the whole file in modem RAM, here the body is
//! streamed straight off the socket. Supports `Content-Length` and chunked
//! transfer encoding, plus read-until-close as a fallback.
//!
//! Generic over the transport so the parser and body reader are host-testable
//! over plain TCP; the driver plugs in an embassy-net socket.

use embedded_io_async_07::{Read, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtaError {
    /// URL not in the supported `http://host[:port]/path` form.
    BadUrl,
    /// The artifact must travel over plain HTTP for now (it is signed and
    /// verified by the bootloader); a TLS OTA session would need a second
    /// set of TLS record buffers.
    HttpsUnsupported,
    /// Transport failure.
    Io,
    /// Malformed response or non-200 status.
    Http,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Url<'a> {
    pub host: &'a str,
    pub port: u16,
    /// Path including the leading slash.
    pub path: &'a str,
}

/// Splits `http://host[:port]/path`. No percent-decoding; the URL comes from
/// the trusted server announcement.
pub fn parse_url(url: &str) -> Result<Url<'_>, OtaError> {
    if let Some(rest) = url.strip_prefix("https://") {
        let _ = rest;
        return Err(OtaError::HttpsUnsupported);
    }
    let rest = url.strip_prefix("http://").ok_or(OtaError::BadUrl)?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| OtaError::BadUrl)?),
        None => (authority, 80),
    };
    if host.is_empty() {
        return Err(OtaError::BadUrl);
    }
    Ok(Url { host, port, path })
}

const HEADER_BUF_LEN: usize = 1024;

enum Body {
    Length { remaining: u64 },
    Chunked(ChunkState),
    UntilClose,
}

enum ChunkState {
    /// Reading the chunk-size line.
    Size,
    /// Reading chunk payload bytes.
    Data { remaining: u64 },
    /// Reading the CRLF after a chunk's payload.
    DataEnd,
    /// Final chunk seen.
    Done,
}

/// A streaming HTTP/1.1 response body.
pub struct HttpBody<T> {
    transport: T,
    body: Body,
    /// Bytes read past the headers (or past the current parse position).
    /// Sized like the header buffer, so the body leftover from the header
    /// read is guaranteed to fit.
    buf: [u8; HEADER_BUF_LEN],
    pos: usize,
    len: usize,
    finished: bool,
}

impl<T: Read + Write> HttpBody<T> {
    /// Sends the GET request and parses the response headers.
    pub async fn open(mut transport: T, host: &str, path: &str) -> Result<Self, OtaError> {
        for part in [
            "GET ",
            path,
            " HTTP/1.1\r\nHost: ",
            host,
            "\r\nConnection: close\r\n\r\n",
        ] {
            transport
                .write_all(part.as_bytes())
                .await
                .map_err(|_| OtaError::Io)?;
        }
        transport.flush().await.map_err(|_| OtaError::Io)?;

        // Read until the end of the headers.
        let mut header = [0u8; HEADER_BUF_LEN];
        let mut filled = 0;
        let header_end = loop {
            if filled == header.len() {
                return Err(OtaError::Http);
            }
            let n = transport
                .read(&mut header[filled..])
                .await
                .map_err(|_| OtaError::Io)?;
            if n == 0 {
                return Err(OtaError::Http);
            }
            filled += n;
            if let Some(i) = find(&header[..filled], b"\r\n\r\n") {
                break i + 4;
            }
        };

        let body = parse_headers(&header[..header_end])?;

        let mut this = Self {
            transport,
            body,
            buf: [0; HEADER_BUF_LEN],
            pos: 0,
            len: 0,
            finished: false,
        };
        // Body bytes already read together with the headers.
        let leftover = filled - header_end;
        this.buf[..leftover].copy_from_slice(&header[header_end..filled]);
        this.len = leftover;
        Ok(this)
    }

    /// Fills `out` completely with body bytes, except at the end of the
    /// body; `Ok(0)` means the body is complete. Full chunks matter: the
    /// caller feeds fixed-size base64 chunks to the decoder.
    pub async fn read(&mut self, out: &mut [u8]) -> Result<usize, OtaError> {
        if self.finished || out.is_empty() {
            return Ok(0);
        }
        let mut written = 0;
        while written < out.len() {
            match self.step(out, &mut written).await? {
                Step::Continue => {}
                Step::Eof => {
                    self.finished = true;
                    break;
                }
            }
        }
        Ok(written)
    }

    /// One parse step over the buffered bytes; refills when empty.
    async fn step(&mut self, out: &mut [u8], written: &mut usize) -> Result<Step, OtaError> {
        if self.pos == self.len {
            self.pos = 0;
            self.len = self
                .transport
                .read(&mut self.buf)
                .await
                .map_err(|_| OtaError::Io)?;
            if self.len == 0 {
                // Peer closed: end of body for UntilClose, error mid-body
                // otherwise.
                return match &self.body {
                    Body::UntilClose => Ok(Step::Eof),
                    Body::Length { remaining: 0 } => Ok(Step::Eof),
                    Body::Chunked(ChunkState::Done) => Ok(Step::Eof),
                    _ => Err(OtaError::Http),
                };
            }
        }
        let available = &self.buf[self.pos..self.len];

        match &mut self.body {
            Body::UntilClose => {
                let n = copy_out(available, out, written);
                self.pos += n;
                Ok(Step::Continue)
            }
            Body::Length { remaining } => {
                if *remaining == 0 {
                    return Ok(Step::Eof);
                }
                let limit = (*remaining).min(available.len() as u64) as usize;
                let n = copy_out(&available[..limit], out, written);
                self.pos += n;
                *remaining -= n as u64;
                if *remaining == 0 {
                    return Ok(Step::Eof);
                }
                Ok(Step::Continue)
            }
            Body::Chunked(state) => match state {
                ChunkState::Done => Ok(Step::Eof),
                ChunkState::Size => {
                    // Need a full size line in the buffer.
                    match find(available, b"\r\n") {
                        Some(i) => {
                            let size = parse_chunk_size(&available[..i])?;
                            self.pos += i + 2;
                            *state = if size == 0 {
                                ChunkState::Done
                            } else {
                                ChunkState::Data { remaining: size }
                            };
                            if size == 0 {
                                return Ok(Step::Eof);
                            }
                            Ok(Step::Continue)
                        }
                        None if self.len == self.buf.len() && self.pos == 0 => {
                            Err(OtaError::Http)
                        }
                        None => self.refill_partial().await,
                    }
                }
                ChunkState::Data { remaining } => {
                    let limit = (*remaining).min(available.len() as u64) as usize;
                    let n = copy_out(&available[..limit], out, written);
                    self.pos += n;
                    *remaining -= n as u64;
                    if *remaining == 0 {
                        *state = ChunkState::DataEnd;
                    }
                    Ok(Step::Continue)
                }
                ChunkState::DataEnd => {
                    if available.len() < 2 {
                        return self.refill_partial().await;
                    }
                    if &available[..2] != b"\r\n" {
                        return Err(OtaError::Http);
                    }
                    self.pos += 2;
                    *state = ChunkState::Size;
                    Ok(Step::Continue)
                }
            },
        }
    }
}

impl<T: Read + Write> HttpBody<T> {
    /// Compacts a partial protocol element to the buffer start and reads more
    /// transport bytes behind it.
    async fn refill_partial(&mut self) -> Result<Step, OtaError> {
        self.buf.copy_within(self.pos..self.len, 0);
        self.len -= self.pos;
        self.pos = 0;
        let n = self
            .transport
            .read(&mut self.buf[self.len..])
            .await
            .map_err(|_| OtaError::Io)?;
        if n == 0 {
            return Err(OtaError::Http);
        }
        self.len += n;
        Ok(Step::Continue)
    }
}

enum Step {
    Continue,
    Eof,
}

fn copy_out(available: &[u8], out: &mut [u8], written: &mut usize) -> usize {
    let n = available.len().min(out.len() - *written);
    out[*written..*written + n].copy_from_slice(&available[..n]);
    *written += n;
    n
}

fn parse_headers(header: &[u8]) -> Result<Body, OtaError> {
    let text = core::str::from_utf8(header).map_err(|_| OtaError::Http)?;
    let mut lines = text.split("\r\n");
    let status = lines.next().ok_or(OtaError::Http)?;
    // "HTTP/1.1 200 OK"
    let code = status
        .split(' ')
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or(OtaError::Http)?;
    if code != 200 {
        log::error!(target: "quectel-ppp", "OTA HTTP status {code}");
        return Err(OtaError::Http);
    }

    let mut body = Body::UntilClose;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            let remaining = value.parse::<u64>().map_err(|_| OtaError::Http)?;
            body = Body::Length { remaining };
        } else if name.eq_ignore_ascii_case("transfer-encoding")
            && value.eq_ignore_ascii_case("chunked")
        {
            return Ok(Body::Chunked(ChunkState::Size));
        }
    }
    Ok(body)
}

fn parse_chunk_size(line: &[u8]) -> Result<u64, OtaError> {
    let text = core::str::from_utf8(line).map_err(|_| OtaError::Http)?;
    // Chunk extensions (";...") are allowed and ignored.
    let digits = text.split(';').next().unwrap_or("").trim();
    u64::from_str_radix(digits, 16).map_err(|_| OtaError::Http)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory transport: canned response bytes served in small pieces to
    /// exercise refill paths; writes recorded.
    struct MemTransport {
        response: std::vec::Vec<u8>,
        pos: usize,
        piece: usize,
        request: std::vec::Vec<u8>,
    }

    impl MemTransport {
        fn new(response: &[u8], piece: usize) -> Self {
            Self {
                response: response.to_vec(),
                pos: 0,
                piece,
                request: std::vec::Vec::new(),
            }
        }
    }

    impl embedded_io_async_07::ErrorType for MemTransport {
        type Error = core::convert::Infallible;
    }
    impl Read for MemTransport {
        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
            let n = self
                .piece
                .min(buf.len())
                .min(self.response.len() - self.pos);
            buf[..n].copy_from_slice(&self.response[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }
    impl Write for MemTransport {
        async fn write(&mut self, data: &[u8]) -> Result<usize, Self::Error> {
            self.request.extend_from_slice(data);
            Ok(data.len())
        }
        async fn flush(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    async fn drain(body: &mut HttpBody<MemTransport>, chunk: usize) -> std::vec::Vec<u8> {
        let mut out = std::vec::Vec::new();
        let mut buf = vec![0u8; chunk];
        loop {
            let n = body.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            // Full chunks except possibly the last (base64 decoding relies
            // on this).
            out.extend_from_slice(&buf[..n]);
            if n < chunk {
                assert_eq!(body.read(&mut buf).await.unwrap(), 0);
                break;
            }
        }
        out
    }

    #[test]
    fn url_parsing() {
        let u = parse_url("http://fw.example.com/dist/app.b64").unwrap();
        assert_eq!((u.host, u.port, u.path), ("fw.example.com", 80, "/dist/app.b64"));
        let u = parse_url("http://10.0.0.1:8080/f").unwrap();
        assert_eq!((u.host, u.port, u.path), ("10.0.0.1", 8080, "/f"));
        let u = parse_url("http://host.example").unwrap();
        assert_eq!((u.host, u.port, u.path), ("host.example", 80, "/"));
        assert_eq!(parse_url("https://x/y"), Err(OtaError::HttpsUnsupported));
        assert_eq!(parse_url("ftp://x/y"), Err(OtaError::BadUrl));
        assert_eq!(parse_url("http://:80/y"), Err(OtaError::BadUrl));
    }

    #[tokio::test]
    async fn content_length_body() {
        let payload: std::vec::Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nServer: t\r\n\r\n",
            payload.len()
        )
        .into_bytes();
        response.extend_from_slice(&payload);

        for piece in [7, 64, 4096] {
            let transport = MemTransport::new(&response, piece);
            let mut body = HttpBody::open(transport, "h", "/p").await.unwrap();
            assert_eq!(drain(&mut body, 400).await, payload);
        }
    }

    #[tokio::test]
    async fn chunked_body() {
        let payload: std::vec::Vec<u8> = (0..900u32).map(|i| (i % 199) as u8).collect();
        let mut response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        for part in payload.chunks(250) {
            response.extend_from_slice(format!("{:x}\r\n", part.len()).as_bytes());
            response.extend_from_slice(part);
            response.extend_from_slice(b"\r\n");
        }
        response.extend_from_slice(b"0\r\n\r\n");

        for piece in [3, 61, 4096] {
            let transport = MemTransport::new(&response, piece);
            let mut body = HttpBody::open(transport, "h", "/p").await.unwrap();
            assert_eq!(drain(&mut body, 400).await, payload);
        }
    }

    #[tokio::test]
    async fn until_close_body() {
        let mut response = b"HTTP/1.1 200 OK\r\n\r\n".to_vec();
        response.extend_from_slice(b"streamed until close");
        let transport = MemTransport::new(&response, 5);
        let mut body = HttpBody::open(transport, "h", "/p").await.unwrap();
        assert_eq!(drain(&mut body, 400).await, b"streamed until close");
    }

    #[tokio::test]
    async fn non_200_is_rejected() {
        let response = b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
        let result = HttpBody::open(MemTransport::new(response, 64), "h", "/p").await;
        assert!(matches!(result, Err(OtaError::Http)));
    }

    #[tokio::test]
    async fn truncated_length_body_errors() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nshort";
        let mut body = HttpBody::open(MemTransport::new(response, 64), "h", "/p")
            .await
            .unwrap();
        let mut buf = [0u8; 400];
        assert_eq!(body.read(&mut buf).await, Err(OtaError::Http));
    }

    #[tokio::test]
    async fn request_shape() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
        let mut body = HttpBody::open(MemTransport::new(response, 64), "fw.example.com", "/a.b64")
            .await
            .unwrap();
        let mut buf = [0u8; 8];
        assert_eq!(body.read(&mut buf).await.unwrap(), 0);
        assert_eq!(
            body.transport.request,
            b"GET /a.b64 HTTP/1.1\r\nHost: fw.example.com\r\nConnection: close\r\n\r\n"
        );
    }
}
