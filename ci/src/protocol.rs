//! Wire framing for the continuity / stall probe.
//!
//! The probe is a pure echo: the client streams framed payloads through the
//! zf-worker forward port, the `npt-ci serve` endpoint (deployed at the forward
//! target) echoes every frame back verbatim. The client matches frames by
//! sequence number and derives RTT from the embedded send timestamp, while a
//! watchdog flags any gap in the returning stream as a stall (断流).
//!
//! TCP frame layout (little-endian):
//!   [u32 magic][u32 payload_len][u64 seq][u64 send_ts_micros][payload bytes]
//!
//! The header is fixed at [`HEADER_LEN`] bytes. The server echoes the exact
//! bytes it read, so the client parses the same layout back.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// "NPT1" — guards against accidentally testing a port that forwards to some
/// unrelated service (the reader bails instead of hanging on garbage).
pub const MAGIC: u32 = 0x3154_504E;

/// magic(4) + payload_len(4) + seq(8) + send_ts_micros(8)
pub const HEADER_LEN: usize = 24;

/// Hard cap on a single frame payload so a corrupted length field can never
/// trigger a huge allocation. 8 MiB is far above any sane probe chunk.
pub const MAX_PAYLOAD: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Frame {
    pub seq: u64,
    pub send_ts_micros: u64,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(HEADER_LEN + self.payload.len());
        buf.extend_from_slice(&MAGIC.to_le_bytes());
        buf.extend_from_slice(&(self.payload.len() as u32).to_le_bytes());
        buf.extend_from_slice(&self.seq.to_le_bytes());
        buf.extend_from_slice(&self.send_ts_micros.to_le_bytes());
        buf.extend_from_slice(&self.payload);
        buf
    }

    /// Read exactly one frame. Returns `Ok(None)` on a clean EOF that lands on a
    /// frame boundary (peer closed normally); any partial frame is an error.
    pub async fn read<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Option<Frame>> {
        let mut header = [0u8; HEADER_LEN];
        match r.read_exact(&mut header).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
        let magic = u32::from_le_bytes(header[0..4].try_into().unwrap());
        if magic != MAGIC {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("bad magic 0x{magic:08X} (not an npt echo endpoint?)"),
            ));
        }
        let len = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
        if len > MAX_PAYLOAD {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("frame payload {len} exceeds MAX_PAYLOAD"),
            ));
        }
        let seq = u64::from_le_bytes(header[8..16].try_into().unwrap());
        let send_ts_micros = u64::from_le_bytes(header[16..24].try_into().unwrap());
        let mut payload = vec![0u8; len];
        r.read_exact(&mut payload).await?;
        Ok(Some(Frame {
            seq,
            send_ts_micros,
            payload,
        }))
    }

    pub async fn write<W: AsyncWrite + Unpin>(&self, w: &mut W) -> std::io::Result<()> {
        w.write_all(&self.encode()).await
    }
}

/// UDP datagram layout: [u32 magic][u64 seq][u64 send_ts_micros][payload].
/// No length prefix — the datagram boundary is the frame boundary.
pub const UDP_HEADER_LEN: usize = 20;

pub fn encode_udp(seq: u64, send_ts_micros: u64, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(UDP_HEADER_LEN + payload.len());
    buf.extend_from_slice(&MAGIC.to_le_bytes());
    buf.extend_from_slice(&seq.to_le_bytes());
    buf.extend_from_slice(&send_ts_micros.to_le_bytes());
    buf.extend_from_slice(payload);
    buf
}

/// Parse a UDP datagram. Returns `(seq, send_ts_micros)`; payload is ignored by
/// the probe (only continuity and RTT matter).
pub fn decode_udp(buf: &[u8]) -> Option<(u64, u64)> {
    if buf.len() < UDP_HEADER_LEN {
        return None;
    }
    let magic = u32::from_le_bytes(buf[0..4].try_into().unwrap());
    if magic != MAGIC {
        return None;
    }
    let seq = u64::from_le_bytes(buf[4..12].try_into().unwrap());
    let ts = u64::from_le_bytes(buf[12..20].try_into().unwrap());
    Some((seq, ts))
}

/// Microseconds since the unix epoch, for RTT stamping.
pub fn now_micros() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}
