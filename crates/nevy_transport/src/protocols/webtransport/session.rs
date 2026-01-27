//! WebTransport session state and stream/datagram framing.

use bytes::{Buf, BufMut, Bytes, BytesMut};
use quinn_proto::StreamId;

/// WebTransport stream type markers
pub const WT_STREAM_BIDI: u8 = 0x41;
pub const WT_STREAM_UNI: u8 = 0x54;

/// State of a WebTransport session within a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Waiting for peer SETTINGS frame
    AwaitingSettings,
    /// SETTINGS received, sending CONNECT request (client) or waiting for CONNECT (server)
    SettingsReceived,
    /// CONNECT sent, awaiting response (client only)
    AwaitingConnectResponse,
    /// Session is fully established
    Established,
    /// Session is closing
    Closing,
    /// Session is closed
    Closed,
}

impl Default for SessionState {
    fn default() -> Self {
        Self::AwaitingSettings
    }
}

/// A WebTransport session within a QUIC connection.
#[derive(Debug)]
pub struct WebTransportSession {
    /// Session ID (quarter_stream_id = request_stream_id / 4)
    pub session_id: u64,
    /// The QUIC stream ID used for the CONNECT request
    pub request_stream_id: Option<StreamId>,
    /// Current session state
    pub state: SessionState,
}

impl WebTransportSession {
    /// Create a new session in the initial state.
    pub fn new() -> Self {
        Self {
            session_id: 0,
            request_stream_id: None,
            state: SessionState::default(),
        }
    }
}

impl Default for WebTransportSession {
    fn default() -> Self {
        Self::new()
    }
}

/// Write a QUIC variable-length integer to a buffer.
pub fn write_varint(buf: &mut BytesMut, value: u64) {
    if value < 64 {
        buf.put_u8(value as u8);
    } else if value < 16384 {
        buf.put_u8(0x40 | ((value >> 8) as u8));
        buf.put_u8(value as u8);
    } else if value < 1_073_741_824 {
        buf.put_u8(0x80 | ((value >> 24) as u8));
        buf.put_u8((value >> 16) as u8);
        buf.put_u8((value >> 8) as u8);
        buf.put_u8(value as u8);
    } else {
        buf.put_u8(0xc0 | ((value >> 56) as u8));
        buf.put_u8((value >> 48) as u8);
        buf.put_u8((value >> 40) as u8);
        buf.put_u8((value >> 32) as u8);
        buf.put_u8((value >> 24) as u8);
        buf.put_u8((value >> 16) as u8);
        buf.put_u8((value >> 8) as u8);
        buf.put_u8(value as u8);
    }
}

/// Read a QUIC variable-length integer from a buffer.
/// Returns None if the buffer doesn't have enough data.
pub fn read_varint(buf: &mut impl Buf) -> Option<u64> {
    if !buf.has_remaining() {
        return None;
    }

    let first = buf.chunk()[0];
    let prefix = first >> 6;
    let len = 1 << prefix;

    if buf.remaining() < len {
        return None;
    }

    let mut value = (first & 0x3f) as u64;
    buf.advance(1);

    for _ in 1..len {
        value = (value << 8) | (buf.get_u8() as u64);
    }

    Some(value)
}

/// Get the encoded length of a varint.
pub fn varint_len(value: u64) -> usize {
    if value < 64 {
        1
    } else if value < 16384 {
        2
    } else if value < 1_073_741_824 {
        4
    } else {
        8
    }
}

/// Frame data for a bidirectional WebTransport stream.
/// Format: 0x41 | session_id (varint) | data
pub fn frame_bidi_stream_header(session_id: u64) -> Bytes {
    let mut buf = BytesMut::with_capacity(1 + varint_len(session_id));
    buf.put_u8(WT_STREAM_BIDI);
    write_varint(&mut buf, session_id);
    buf.freeze()
}

/// Frame data for a unidirectional WebTransport stream.
/// Format: 0x54 | session_id (varint) | data
pub fn frame_uni_stream_header(session_id: u64) -> Bytes {
    let mut buf = BytesMut::with_capacity(1 + varint_len(session_id));
    buf.put_u8(WT_STREAM_UNI);
    write_varint(&mut buf, session_id);
    buf.freeze()
}

/// Frame a datagram with the quarter_stream_id prefix.
/// Format: quarter_stream_id (varint) | payload
pub fn frame_datagram(session_id: u64, data: &[u8]) -> Bytes {
    let mut buf = BytesMut::with_capacity(varint_len(session_id) + data.len());
    write_varint(&mut buf, session_id);
    buf.extend_from_slice(data);
    buf.freeze()
}

/// Parse a datagram, extracting the session_id and payload.
/// Returns (session_id, payload) or None if parsing fails.
pub fn parse_datagram(mut data: Bytes) -> Option<(u64, Bytes)> {
    let session_id = read_varint(&mut data)?;
    Some((session_id, data))
}

/// Parse a WebTransport stream header.
/// Returns (stream_type, session_id, bytes_consumed) or None if parsing fails.
/// Note: stream_type is encoded as a varint in the WebTransport protocol.
pub fn parse_stream_header(mut data: &[u8]) -> Option<(u64, u64, usize)> {
    let start_len = data.len();

    let stream_type = read_varint(&mut data)?;
    let session_id = read_varint(&mut data)?;
    let consumed = start_len - data.len();

    Some((stream_type, session_id, consumed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_varint_roundtrip() {
        let test_values = [0u64, 63, 64, 16383, 16384, 1_073_741_823, 1_073_741_824];

        for value in test_values {
            let mut buf = BytesMut::new();
            write_varint(&mut buf, value);
            let mut bytes = buf.freeze();
            let decoded = read_varint(&mut bytes).unwrap();
            assert_eq!(value, decoded, "Failed for value {}", value);
        }
    }

    #[test]
    fn test_frame_bidi_stream() {
        let header = frame_bidi_stream_header(42);
        assert_eq!(header[0], WT_STREAM_BIDI);
    }

    #[test]
    fn test_frame_datagram() {
        let framed = frame_datagram(5, b"hello");
        let (session_id, payload) = parse_datagram(framed).unwrap();
        assert_eq!(session_id, 5);
        assert_eq!(&payload[..], b"hello");
    }
}
