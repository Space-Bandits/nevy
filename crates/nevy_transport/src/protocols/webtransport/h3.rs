//! HTTP/3 protocol layer for WebTransport.
//!
//! This module handles:
//! - Control stream setup and SETTINGS exchange
//! - CONNECT request/response for WebTransport session establishment
//! - QPACK header encoding/decoding

use bytes::{Buf, BufMut, Bytes, BytesMut};
use qpack::{decode_stateless, encode_stateless, HeaderField};

use super::session::{read_varint, write_varint};

/// H3 frame types
const H3_FRAME_HEADERS: u64 = 0x01;
const H3_FRAME_SETTINGS: u64 = 0x04;

/// H3 stream types
const H3_STREAM_CONTROL: u8 = 0x00;

/// H3 settings identifiers
const H3_SETTINGS_QPACK_MAX_TABLE_CAPACITY: u64 = 0x01;
const H3_SETTINGS_QPACK_BLOCKED_STREAMS: u64 = 0x07;
const H3_SETTINGS_ENABLE_CONNECT_PROTOCOL: u64 = 0x08;
const H3_SETTINGS_H3_DATAGRAM: u64 = 0x33;
const H3_SETTINGS_ENABLE_WEBTRANSPORT: u64 = 0x2b603742;

/// H3 error codes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum H3Error {
    /// Frame parsing error
    FrameError,
    /// Invalid CONNECT response
    InvalidConnectResponse,
    /// QPACK encoding/decoding error
    QpackError,
}

/// Peer's H3 settings.
#[derive(Debug, Default, Clone)]
pub struct H3Settings {
    pub qpack_max_table_capacity: u64,
    pub qpack_blocked_streams: u64,
    pub enable_connect_protocol: bool,
    pub enable_datagrams: bool,
    pub enable_webtransport: bool,
}

impl H3Settings {
    /// Check if the peer supports WebTransport.
    pub fn supports_webtransport(&self) -> bool {
        // Note: enable_connect_protocol (0x08) is an HTTP/2 setting.
        // Chrome doesn't send it for HTTP/3 WebTransport - it just sends
        // enable_webtransport and enable_datagrams.
        self.enable_webtransport && self.enable_datagrams
    }
}

/// H3 control stream state.
#[derive(Debug)]
pub struct H3ControlStream {
    /// Whether we've received peer SETTINGS
    pub settings_received: bool,
    /// Peer's settings
    pub peer_settings: H3Settings,
    /// Buffer for partial frame data
    recv_buffer: BytesMut,
}

impl H3ControlStream {
    /// Create a new control stream state.
    pub fn new() -> Self {
        Self {
            settings_received: false,
            peer_settings: H3Settings::default(),
            recv_buffer: BytesMut::new(),
        }
    }

    /// Create the control stream type byte + SETTINGS frame.
    ///
    /// This should be written to a newly opened unidirectional stream.
    pub fn create_settings_frame() -> Bytes {
        let mut buf = BytesMut::new();

        // Stream type: control (0x00)
        buf.put_u8(H3_STREAM_CONTROL);

        // Build settings payload
        let mut settings_payload = BytesMut::new();

        // QPACK max table capacity = 0 (no dynamic table)
        write_varint(&mut settings_payload, H3_SETTINGS_QPACK_MAX_TABLE_CAPACITY);
        write_varint(&mut settings_payload, 0);

        // QPACK blocked streams = 0
        write_varint(&mut settings_payload, H3_SETTINGS_QPACK_BLOCKED_STREAMS);
        write_varint(&mut settings_payload, 0);

        // Enable CONNECT protocol
        write_varint(&mut settings_payload, H3_SETTINGS_ENABLE_CONNECT_PROTOCOL);
        write_varint(&mut settings_payload, 1);

        // Enable H3 datagrams
        write_varint(&mut settings_payload, H3_SETTINGS_H3_DATAGRAM);
        write_varint(&mut settings_payload, 1);

        // Enable WebTransport
        write_varint(&mut settings_payload, H3_SETTINGS_ENABLE_WEBTRANSPORT);
        write_varint(&mut settings_payload, 1);

        // Write SETTINGS frame header
        write_varint(&mut buf, H3_FRAME_SETTINGS);
        write_varint(&mut buf, settings_payload.len() as u64);
        buf.extend_from_slice(&settings_payload);

        buf.freeze()
    }

    /// Process received data on the control stream.
    ///
    /// Returns Ok(true) if SETTINGS were successfully received.
    pub fn process_received(&mut self, data: Bytes) -> Result<bool, H3Error> {
        self.recv_buffer.extend_from_slice(&data);

        // Skip stream type byte if this is the first data
        if !self.settings_received && !self.recv_buffer.is_empty() {
            if self.recv_buffer[0] == H3_STREAM_CONTROL {
                self.recv_buffer.advance(1);
            }
        }

        // Try to parse SETTINGS frame
        if self.recv_buffer.len() < 2 {
            return Ok(false);
        }

        let mut cursor = &self.recv_buffer[..];
        let frame_type = match read_varint(&mut cursor) {
            Some(t) => t,
            None => return Ok(false),
        };

        let frame_len = match read_varint(&mut cursor) {
            Some(l) => l as usize,
            None => return Ok(false),
        };

        let header_len = self.recv_buffer.len() - cursor.len();

        if cursor.len() < frame_len {
            return Ok(false);
        }

        if frame_type == H3_FRAME_SETTINGS {
            // Copy settings data to avoid borrow conflict
            let settings_data = cursor[..frame_len].to_vec();

            // Remove processed data first
            self.recv_buffer.advance(header_len + frame_len);

            // Now parse settings
            self.parse_settings(&settings_data)?;
            self.settings_received = true;

            return Ok(true);
        }

        // Unknown frame type, skip it
        self.recv_buffer.advance(header_len + frame_len);
        Ok(false)
    }

    fn parse_settings(&mut self, mut data: &[u8]) -> Result<(), H3Error> {
        log::info!("Parsing SETTINGS frame, {} bytes", data.len());
        while !data.is_empty() {
            let setting_id = read_varint(&mut data).ok_or(H3Error::FrameError)?;
            let value = read_varint(&mut data).ok_or(H3Error::FrameError)?;

            log::info!("  Setting 0x{:x} = {}", setting_id, value);

            match setting_id {
                H3_SETTINGS_QPACK_MAX_TABLE_CAPACITY => {
                    self.peer_settings.qpack_max_table_capacity = value;
                }
                H3_SETTINGS_QPACK_BLOCKED_STREAMS => {
                    self.peer_settings.qpack_blocked_streams = value;
                }
                H3_SETTINGS_ENABLE_CONNECT_PROTOCOL => {
                    self.peer_settings.enable_connect_protocol = value != 0;
                }
                H3_SETTINGS_H3_DATAGRAM => {
                    self.peer_settings.enable_datagrams = value != 0;
                }
                H3_SETTINGS_ENABLE_WEBTRANSPORT => {
                    self.peer_settings.enable_webtransport = value != 0;
                }
                _ => {
                    log::debug!("  Unknown setting 0x{:x}, ignoring", setting_id);
                }
            }
        }

        log::info!("Parsed settings: connect={}, datagrams={}, webtransport={}",
            self.peer_settings.enable_connect_protocol,
            self.peer_settings.enable_datagrams,
            self.peer_settings.enable_webtransport);

        Ok(())
    }
}

impl Default for H3ControlStream {
    fn default() -> Self {
        Self::new()
    }
}

/// Encode a CONNECT request for WebTransport session establishment.
///
/// Returns the HEADERS frame to send on a client-initiated bidirectional stream.
pub fn encode_connect_request(authority: &str, path: &str) -> Result<Bytes, H3Error> {
    let headers = vec![
        HeaderField::new(b":method", b"CONNECT"),
        HeaderField::new(b":protocol", b"webtransport"),
        HeaderField::new(b":scheme", b"https"),
        HeaderField::new(b":authority", authority.as_bytes()),
        HeaderField::new(b":path", path.as_bytes()),
    ];

    let mut header_block = Vec::new();
    encode_stateless(&mut header_block, &headers).map_err(|_| H3Error::QpackError)?;

    // Build HEADERS frame
    let mut buf = BytesMut::new();
    write_varint(&mut buf, H3_FRAME_HEADERS);
    write_varint(&mut buf, header_block.len() as u64);
    buf.extend_from_slice(&header_block);

    Ok(buf.freeze())
}

/// Encode a successful CONNECT response (200 OK).
///
/// Returns the HEADERS frame to send as a response to a CONNECT request.
pub fn encode_connect_response() -> Result<Bytes, H3Error> {
    let headers = vec![HeaderField::new(b":status", b"200")];

    let mut header_block = Vec::new();
    encode_stateless(&mut header_block, &headers).map_err(|_| H3Error::QpackError)?;

    // Build HEADERS frame
    let mut buf = BytesMut::new();
    write_varint(&mut buf, H3_FRAME_HEADERS);
    write_varint(&mut buf, header_block.len() as u64);
    buf.extend_from_slice(&header_block);

    Ok(buf.freeze())
}

/// Parse a CONNECT response to extract the status code.
pub fn parse_connect_response(data: &[u8]) -> Result<u16, H3Error> {
    let mut cursor = data;

    // Read frame header
    let frame_type = read_varint(&mut cursor).ok_or(H3Error::FrameError)?;
    if frame_type != H3_FRAME_HEADERS {
        return Err(H3Error::InvalidConnectResponse);
    }

    let frame_len = read_varint(&mut cursor).ok_or(H3Error::FrameError)? as usize;
    if cursor.len() < frame_len {
        return Err(H3Error::FrameError);
    }

    let header_block = &cursor[..frame_len];

    // Decode QPACK headers
    let mut header_buf = std::io::Cursor::new(header_block);
    let decoded = decode_stateless(&mut header_buf, header_block.len() as u64)
        .map_err(|_| H3Error::QpackError)?;

    // Find :status header
    for field in decoded.fields {
        if field.name.as_ref() == b":status" {
            let status_str =
                std::str::from_utf8(&field.value).map_err(|_| H3Error::InvalidConnectResponse)?;
            let status: u16 = status_str
                .parse()
                .map_err(|_| H3Error::InvalidConnectResponse)?;
            return Ok(status);
        }
    }

    Err(H3Error::InvalidConnectResponse)
}

/// Parse an incoming CONNECT request.
/// Returns (authority, path, protocol) if valid.
pub fn parse_connect_request(data: &[u8]) -> Result<(String, String, String), H3Error> {
    log::info!("parse_connect_request: {} bytes, first 20: {:02x?}", data.len(), &data[..data.len().min(20)]);
    let mut cursor = data;

    // Read frame header
    let frame_type = read_varint(&mut cursor).ok_or(H3Error::FrameError)?;
    log::info!("Frame type: 0x{:x}", frame_type);
    if frame_type != H3_FRAME_HEADERS {
        return Err(H3Error::InvalidConnectResponse);
    }

    let frame_len = read_varint(&mut cursor).ok_or(H3Error::FrameError)? as usize;
    log::info!("Frame length: {}", frame_len);
    if cursor.len() < frame_len {
        return Err(H3Error::FrameError);
    }

    let header_block = &cursor[..frame_len];
    log::info!("Header block: {} bytes, first 20: {:02x?}", header_block.len(), &header_block[..header_block.len().min(20)]);

    // Decode QPACK headers (allow up to 16KB for decoded headers)
    let mut header_buf = std::io::Cursor::new(header_block);
    let decoded = match decode_stateless(&mut header_buf, 16384) {
        Ok(d) => d,
        Err(e) => {
            log::error!("QPACK decode error: {:?}", e);
            return Err(H3Error::QpackError);
        }
    };

    let mut method = None;
    let mut protocol = None;
    let mut authority = None;
    let mut path = None;

    for field in decoded.fields {
        let value_str =
            std::str::from_utf8(&field.value).map_err(|_| H3Error::InvalidConnectResponse)?;
        match field.name.as_ref() {
            b":method" => method = Some(value_str.to_string()),
            b":protocol" => protocol = Some(value_str.to_string()),
            b":authority" => authority = Some(value_str.to_string()),
            b":path" => path = Some(value_str.to_string()),
            _ => {}
        }
    }

    // Validate it's a WebTransport CONNECT
    if method.as_deref() != Some("CONNECT") {
        return Err(H3Error::InvalidConnectResponse);
    }

    let protocol = protocol.ok_or(H3Error::InvalidConnectResponse)?;
    let authority = authority.ok_or(H3Error::InvalidConnectResponse)?;
    let path = path.ok_or(H3Error::InvalidConnectResponse)?;

    Ok((authority, path, protocol))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_settings_frame() {
        let frame = H3ControlStream::create_settings_frame();
        // Should start with control stream type byte
        assert_eq!(frame[0], H3_STREAM_CONTROL);
    }

    #[test]
    fn test_settings_roundtrip() {
        let frame = H3ControlStream::create_settings_frame();

        let mut control = H3ControlStream::new();
        let result = control.process_received(frame);
        assert!(result.is_ok());
        assert!(result.unwrap());
        assert!(control.settings_received);
        assert!(control.peer_settings.enable_webtransport);
        assert!(control.peer_settings.enable_datagrams);
        assert!(control.peer_settings.enable_connect_protocol);
    }
}
