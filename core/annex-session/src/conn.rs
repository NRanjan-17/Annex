//! Framed record I/O over any async byte stream (TLS in practice).

use annex_proto::{Control, RECORD_HEADER_LEN, RecordHeader, RecordType, VideoFrame};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::SessionError;

pub(crate) enum Incoming {
    Control(Control),
    Video(VideoFrame),
}

pub(crate) struct Reader<R> {
    inner: R,
    buf: Vec<u8>,
}

impl<R: AsyncRead + Unpin> Reader<R> {
    pub fn new(inner: R) -> Self {
        Reader { inner, buf: Vec::new() }
    }

    /// Reads the next record. Unknown control message types are skipped.
    pub async fn next(&mut self) -> Result<Incoming, SessionError> {
        loop {
            let mut h = [0u8; RECORD_HEADER_LEN];
            match self.inner.read_exact(&mut h).await {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Err(SessionError::Closed),
                Err(e) => return Err(e.into()),
            }
            let header = RecordHeader::parse(&h)?;
            self.buf.resize(header.len, 0);
            self.inner.read_exact(&mut self.buf).await?;
            match header.kind {
                RecordType::Control => {
                    if let Some(msg) = Control::decode(&self.buf)? {
                        return Ok(Incoming::Control(msg));
                    }
                    tracing::debug!("skipping unknown control message");
                }
                RecordType::Video => return Ok(Incoming::Video(VideoFrame::decode(&self.buf)?)),
            }
        }
    }

    /// Reads records until a control message arrives; video before it is a violation.
    pub async fn next_control(&mut self) -> Result<Control, SessionError> {
        match self.next().await? {
            Incoming::Control(c) => Ok(c),
            Incoming::Video(_) => Err(SessionError::Violation("unexpected video".into())),
        }
    }
}

pub(crate) struct Writer<W> {
    inner: W,
    buf: Vec<u8>,
}

impl<W: AsyncWrite + Unpin> Writer<W> {
    pub fn new(inner: W) -> Self {
        Writer { inner, buf: Vec::new() }
    }

    pub async fn send_control(&mut self, msg: &Control) -> Result<(), SessionError> {
        self.send(RecordType::Control, &msg.encode()).await
    }

    pub async fn send_video(&mut self, header: annex_proto::VideoHeader, data: &[u8]) -> Result<(), SessionError> {
        self.buf.clear();
        let len = annex_proto::VIDEO_HEADER_LEN + data.len();
        if len > annex_proto::MAX_RECORD_LEN {
            return Err(annex_proto::ProtoError::RecordTooLarge(len).into());
        }
        self.buf.extend_from_slice(&RecordHeader { kind: RecordType::Video, len }.to_bytes());
        self.buf.extend_from_slice(&header.to_bytes());
        self.buf.extend_from_slice(data);
        self.inner.write_all(&self.buf).await?;
        self.inner.flush().await?;
        Ok(())
    }

    async fn send(&mut self, kind: RecordType, payload: &[u8]) -> Result<(), SessionError> {
        self.buf.clear();
        annex_proto::encode_record(kind, payload, &mut self.buf)?;
        self.inner.write_all(&self.buf).await?;
        self.inner.flush().await?;
        Ok(())
    }

    pub async fn shutdown(&mut self) {
        let _ = self.inner.shutdown().await;
    }
}

pub(crate) fn hello(core: &crate::Core, roles: Vec<annex_proto::Role>, knows_peer: bool, caps: &[&str]) -> Control {
    Control::Hello(annex_proto::Hello {
        proto_major: annex_proto::PROTOCOL_MAJOR,
        proto_minor: annex_proto::PROTOCOL_MINOR,
        device_name: core.device_name().to_string(),
        device_id: core.fingerprint().to_string(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        roles,
        caps: caps.iter().map(|c| c.to_string()).collect(),
        knows_peer,
    })
}

/// Validates the peer's Hello against the TLS-authenticated fingerprint.
pub(crate) fn check_hello(msg: Control, tls_fingerprint: &str) -> Result<annex_proto::Hello, SessionError> {
    let Control::Hello(h) = msg else {
        return Err(SessionError::Violation("expected hello".into()));
    };
    if h.proto_major != annex_proto::PROTOCOL_MAJOR {
        return Err(SessionError::Violation(format!("unsupported protocol {}", h.proto_major)));
    }
    if h.device_id != tls_fingerprint {
        return Err(SessionError::Violation("hello device_id does not match certificate".into()));
    }
    Ok(h)
}
