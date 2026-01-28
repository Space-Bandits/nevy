//! WebTransport connection context implementation.

use std::net::UdpSocket;

use bevy::prelude::*;
use bytes::Bytes;
use log::warn;
use quinn_proto::{Dir, ReadError, ReadableError, SendDatagramError, VarInt};
use quinn_udp::UdpSocketState;
use thiserror::Error;

use crate::{
    Connection, ConnectionContext, Stream, StreamId, StreamReadError, StreamRequirements,
    protocols::quic::{
        connection::{CloseFlagState, QuicConnectionState},
        udp_transmit,
    },
};

use super::session::{
    frame_bidi_stream_header, frame_datagram, frame_uni_stream_header, parse_datagram,
    parse_stream_header, WebTransportSession, WT_STREAM_BIDI, WT_STREAM_UNI,
};

/// WebTransport stream identifier.
#[derive(Clone, Copy, Debug)]
pub enum WebTransportStreamId {
    /// Stream for sending datagrams.
    Datagrams,
    /// A bidirectional or unidirectional stream.
    Stream {
        quic_stream_id: quinn_proto::StreamId,
        session_id: u64,
        /// Whether the WT framing header has been written/read
        header_handled: bool,
    },
    /// A received datagram (each datagram is a separate "stream").
    ReceivedDatagram(usize),
}

impl StreamId for WebTransportStreamId {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn clone(&self) -> Stream {
        Stream::new(<Self as Clone>::clone(self))
    }

    fn eq(&self, rhs: &Stream) -> bool {
        match (self, rhs.as_stream::<Self>()) {
            (_, Err(_)) => false,
            (Self::Datagrams, Ok(Self::Datagrams)) => true,
            (
                Self::Stream {
                    quic_stream_id: id1,
                    ..
                },
                Ok(Self::Stream {
                    quic_stream_id: id2,
                    ..
                }),
            ) => id1 == id2,
            (Self::ReceivedDatagram(i1), Ok(Self::ReceivedDatagram(i2))) => i1 == i2,
            _ => false,
        }
    }
}

/// Connection context for WebTransport operations.
pub struct WebTransportConnectionContext<'a> {
    pub(crate) connection: &'a mut QuicConnectionState,
    pub(crate) session: &'a mut WebTransportSession,
    pub(crate) send_buffer: &'a mut Vec<u8>,
    pub(crate) max_datagrams: usize,
    pub(crate) socket: &'a UdpSocket,
    pub(crate) socket_state: &'a UdpSocketState,
}

impl<'a> WebTransportConnectionContext<'a> {
    /// Creates a new context with a smaller lifetime.
    pub fn reborrow<'b>(&'b mut self) -> WebTransportConnectionContext<'b> {
        WebTransportConnectionContext {
            connection: self.connection,
            session: self.session,
            send_buffer: self.send_buffer,
            max_datagrams: self.max_datagrams,
            socket: self.socket,
            socket_state: self.socket_state,
        }
    }

    /// Transmits any outstanding packets.
    pub(crate) fn transmit_packets(&mut self) {
        while let Some(transmit) = {
            self.send_buffer.clear();
            self.connection.connection.poll_transmit(
                std::time::Instant::now(),
                self.max_datagrams,
                self.send_buffer,
            )
        } {
            let _ = self.socket_state.send(
                quinn_udp::UdpSockRef::from(self.socket),
                &udp_transmit(&transmit, self.send_buffer),
            );
        }
    }
}

impl<'a> Drop for WebTransportConnectionContext<'a> {
    fn drop(&mut self) {
        self.transmit_packets();
    }
}

impl<'a> ConnectionContext for WebTransportConnectionContext<'a> {
    fn reborrow<'b>(&'b mut self) -> Connection<'b> {
        Connection(Box::new(WebTransportConnectionContext {
            connection: self.connection,
            session: self.session,
            send_buffer: self.send_buffer,
            max_datagrams: self.max_datagrams,
            socket: self.socket,
            socket_state: self.socket_state,
        }))
    }

    fn new_stream(&mut self, requirements: StreamRequirements) -> Result<Stream> {
        match requirements {
            StreamRequirements {
                ordered: false,
                reliable: false,
                ..
            } => Ok(Stream::new(WebTransportStreamId::Datagrams)),
            StreamRequirements { bidirectional, .. } => {
                let dir = if bidirectional { Dir::Bi } else { Dir::Uni };
                let stream_id = self
                    .connection
                    .connection
                    .streams()
                    .open(dir)
                    .ok_or(ExhaustedStreamsError)?;

                // Write the WebTransport framing header
                let header = if bidirectional {
                    frame_bidi_stream_header(self.session.session_id)
                } else {
                    frame_uni_stream_header(self.session.session_id)
                };

                self.connection
                    .connection
                    .send_stream(stream_id)
                    .write(&header)?;

                Ok(Stream::new(WebTransportStreamId::Stream {
                    quic_stream_id: stream_id,
                    session_id: self.session.session_id,
                    header_handled: true,
                }))
            }
        }
    }

    fn write(&mut self, stream: &Stream, data: Bytes, block: bool) -> Result<usize> {
        Ok(match stream.as_stream::<WebTransportStreamId>()? {
            WebTransportStreamId::Datagrams => {
                let framed = frame_datagram(self.session.session_id, &data);
                let len = data.len();

                match self.connection.connection.datagrams().send(framed, !block) {
                    Ok(()) => len,
                    Err(SendDatagramError::Blocked(_)) => 0,
                    Err(err) => return Err(err.into()),
                }
            }
            WebTransportStreamId::Stream {
                quic_stream_id, ..
            } => {
                self.connection
                    .connection
                    .send_stream(*quic_stream_id)
                    .write(data.as_ref())?
            }
            WebTransportStreamId::ReceivedDatagram(_) => 0,
        })
    }

    fn read(&mut self, stream: &Stream) -> Result<Result<Bytes, StreamReadError>> {
        match stream.as_stream::<WebTransportStreamId>()? {
            WebTransportStreamId::Datagrams => Err(SendDatagramReadError.into()),
            WebTransportStreamId::Stream {
                quic_stream_id, ..
            } => {
                let mut stream = self.connection.connection.recv_stream(*quic_stream_id);
                let mut chunks = match stream.read(true) {
                    Ok(chunks) => chunks,
                    Err(ReadableError::ClosedStream) => return Ok(Err(StreamReadError::Closed)),
                    Err(ReadableError::IllegalOrderedRead) => {
                        return Err("Illegal ordered read should never be reached".into());
                    }
                };

                Ok(match chunks.next(usize::MAX) {
                    Ok(Some(chunk)) => Ok(chunk.bytes),
                    Err(ReadError::Blocked) => Err(StreamReadError::Blocked),
                    Err(ReadError::Reset(_)) | Ok(None) => Err(StreamReadError::Closed),
                })
            }
            WebTransportStreamId::ReceivedDatagram(index) => {
                let Some(idx) = index.checked_sub(self.connection.datagrams_queue_offset) else {
                    return Ok(Err(StreamReadError::Closed));
                };

                let Some(datagram) = self.connection.datagrams.get_mut(idx) else {
                    return Err("Invalid datagram index".into());
                };

                let result = datagram.take().ok_or(StreamReadError::Closed);

                // Clean up consumed datagrams from the front
                while let Some(None) = self.connection.datagrams.front() {
                    self.connection.datagrams.pop_front();
                    self.connection.datagrams_queue_offset += 1;
                }

                Ok(result)
            }
        }
    }

    fn close_send_stream(&mut self, stream: &Stream, graceful: bool) -> Result {
        match stream.as_stream::<WebTransportStreamId>()? {
            WebTransportStreamId::Datagrams => (),
            WebTransportStreamId::Stream {
                quic_stream_id, ..
            } => {
                let mut stream = self.connection.connection.send_stream(*quic_stream_id);

                match graceful {
                    true => {
                        if let Err(err) = stream.finish() {
                            warn!("Failed to finish stream {}: {}", quic_stream_id, err);
                        }
                    }
                    false => {
                        if let Err(err) = stream.reset(VarInt::from_u32(0)) {
                            warn!("Failed to reset stream {}: {}", quic_stream_id, err);
                        }
                    }
                }
            }
            WebTransportStreamId::ReceivedDatagram(_) => (),
        };

        Ok(())
    }

    fn close_recv_stream(&mut self, stream: &Stream) -> Result {
        match stream.as_stream::<WebTransportStreamId>()? {
            WebTransportStreamId::Datagrams => (),
            WebTransportStreamId::Stream {
                quic_stream_id, ..
            } => {
                if let Err(err) = self
                    .connection
                    .connection
                    .recv_stream(*quic_stream_id)
                    .stop(VarInt::from_u32(0))
                {
                    warn!("Failed to stop stream {}: {}", quic_stream_id, err);
                }
            }
            WebTransportStreamId::ReceivedDatagram(_) => (),
        };

        Ok(())
    }

    fn accept_stream(&mut self) -> Option<(Stream, StreamRequirements)> {
        // Check for incoming unidirectional streams
        if let Some(stream_id) = self.connection.connection.streams().accept(Dir::Uni) {
            // Read and verify WT framing header
            let mut stream = self.connection.connection.recv_stream(stream_id);
            if let Ok(mut chunks) = stream.read(true) {
                if let Ok(Some(chunk)) = chunks.next(16) {
                    if let Some((stream_type, session_id, _consumed)) =
                        parse_stream_header(&chunk.bytes)
                    {
                        if session_id == self.session.session_id && stream_type == WT_STREAM_UNI as u64 {
                            return Some((
                                Stream::new(WebTransportStreamId::Stream {
                                    quic_stream_id: stream_id,
                                    session_id,
                                    header_handled: true,
                                }),
                                StreamRequirements::RELIABLE_ORDERED,
                            ));
                        }
                    }
                }
            }
        }

        // Check for incoming bidirectional streams
        if let Some(stream_id) = self.connection.connection.streams().accept(Dir::Bi) {
            log::info!("accept_stream: Got bidi stream {:?}, our session_id={}", stream_id, self.session.session_id);
            // Read and verify WT framing header
            let mut stream = self.connection.connection.recv_stream(stream_id);
            if let Ok(mut chunks) = stream.read(true) {
                if let Ok(Some(chunk)) = chunks.next(16) {
                    log::info!("accept_stream: Read {} bytes from stream: {:02x?}", chunk.bytes.len(), &chunk.bytes[..chunk.bytes.len().min(16)]);
                    if let Some((stream_type, session_id, consumed)) =
                        parse_stream_header(&chunk.bytes)
                    {
                        log::info!("accept_stream: Parsed header - type=0x{:x}, session_id={}, consumed={}", stream_type, session_id, consumed);
                        if session_id == self.session.session_id && stream_type == WT_STREAM_BIDI as u64 {
                            return Some((
                                Stream::new(WebTransportStreamId::Stream {
                                    quic_stream_id: stream_id,
                                    session_id,
                                    header_handled: true,
                                }),
                                StreamRequirements::RELIABLE_ORDERED.with_bidirectional(true),
                            ));
                        } else {
                            log::warn!("accept_stream: Session/type mismatch - expected session={}, type=0x41", self.session.session_id);
                        }
                    } else {
                        log::warn!("accept_stream: Failed to parse WT stream header");
                    }
                } else {
                    log::debug!("accept_stream: No data ready on stream yet");
                }
            } else {
                log::warn!("accept_stream: Failed to read from stream");
            }
        }

        // Check for incoming datagrams
        if let Some(datagram) = self.connection.connection.datagrams().recv() {
            // Parse the datagram to extract session_id and payload
            if let Some((session_id, payload)) = parse_datagram(datagram) {
                if session_id == self.session.session_id {
                    let index =
                        self.connection.datagrams_queue_offset + self.connection.datagrams.len();

                    self.connection.datagrams.push_back(Some(payload));

                    return Some((
                        Stream::new(WebTransportStreamId::ReceivedDatagram(index)),
                        StreamRequirements::UNRELIABLE,
                    ));
                }
            }
        }

        None
    }

    fn close(&mut self) {
        if let CloseFlagState::None = self.connection.close {
            self.connection.close = CloseFlagState::Sent;
        }
    }

    fn all_data_sent(&mut self) -> bool {
        self.connection.connection.streams().send_streams() == 0
    }
}

#[derive(Error, Debug)]
#[error("WebTransport streams in the desired direction are exhausted.")]
pub struct ExhaustedStreamsError;

#[derive(Error, Debug)]
#[error("This WebTransport datagram stream is only for sending.")]
pub struct SendDatagramReadError;
