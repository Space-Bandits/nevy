use std::{collections::VecDeque, net::UdpSocket};

use bevy::prelude::*;
use bytes::Bytes;
use log::warn;
use quinn_proto::{Dir, ReadError, ReadableError, SendDatagramError, StreamEvent, VarInt};
use quinn_udp::UdpSocketState;
use thiserror::Error;

use crate::{
    Connection, ConnectionContext, Stream, StreamId, StreamReadError, StreamRequirements,
    protocols::quic::udp_transmit,
};

pub(crate) struct QuicConnectionState {
    pub connection_entity: Entity,
    pub connection: quinn_proto::Connection,
    pub stream_events: VecDeque<StreamEvent>,
    /// signal to close the connection on the next update
    pub close: CloseFlagState,
    pub datagrams_queue_offset: usize,
    pub datagrams: VecDeque<Option<Bytes>>,
}

pub(crate) enum CloseFlagState {
    None,
    Sent,
    Received,
}

/// A quic stream id that gets type erased by [`Stream`].
#[derive(Clone, Copy)]
pub enum QuicStreamId {
    /// Stream id for sending datagrams on.
    Datagrams,
    /// Quic stream id.
    Stream(quinn_proto::StreamId),
    /// Each received datagram is considered a different stream.
    ReceivedDatagram(usize),
}

impl StreamId for QuicStreamId {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn clone(&self) -> crate::Stream {
        Stream::new(<Self as Clone>::clone(self))
    }

    fn eq(&self, rhs: &Stream) -> bool {
        match (self, rhs.as_stream::<Self>()) {
            (_, Err(_)) => false,
            (Self::Datagrams, Ok(Self::Datagrams)) => true,
            (Self::Stream(id1), Ok(Self::Stream(id2))) => id1 == id2,
            _ => false,
        }
    }
}

/// Gets wrapped in a [`Connection`] and contains the context needed for performing type erased operations on a QUIC connection.
///
/// When dropped will attempt to transmit outstanding data.
pub struct QuicConnectionContext<'a> {
    pub(crate) connection: &'a mut QuicConnectionState,
    pub(crate) send_buffer: &'a mut Vec<u8>,
    pub(crate) max_datagrams: usize,
    pub(crate) socket: &'a UdpSocket,
    pub(crate) socket_state: &'a UdpSocketState,
}

impl<'a> QuicConnectionContext<'a> {
    /// Creates a new [`QuicConnectionContext`] with a smaller lifetime
    pub fn reborrow<'b>(&'b mut self) -> QuicConnectionContext<'b> {
        QuicConnectionContext {
            connection: self.connection,
            send_buffer: self.send_buffer,
            max_datagrams: self.max_datagrams,
            socket: self.socket,
            socket_state: self.socket_state,
        }
    }

    /// Transmits any outstanding packets.
    ///
    /// This method is called whenever a [`QuicConnectionContext`] is dropped and during the [`UpdateEndpointSystems`](crate::UpdateEndpointSystems) system set.
    pub(crate) fn transmit_packets(&mut self) {
        while let Some(transmit) = {
            self.send_buffer.clear();

            self.connection.connection.poll_transmit(
                std::time::Instant::now(),
                self.max_datagrams,
                &mut self.send_buffer,
            )
        } {
            // the transmit failing is equivelant to dropping due to congestion, ignore error
            let _ = self.socket_state.send(
                quinn_udp::UdpSockRef::from(&self.socket),
                &udp_transmit(&transmit, &self.send_buffer),
            );
        }
    }
}

impl<'a> Drop for QuicConnectionContext<'a> {
    fn drop(&mut self) {
        self.transmit_packets();
    }
}

impl<'a> ConnectionContext for QuicConnectionContext<'a> {
    fn reborrow<'b>(&'b mut self) -> Connection<'b> {
        Connection(Box::new(QuicConnectionContext {
            connection: self.connection,
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
            } => Ok(Stream::new(QuicStreamId::Datagrams)),
            StreamRequirements { bidirectional, .. } => {
                let stream_id = self
                    .connection
                    .connection
                    .streams()
                    .open(match bidirectional {
                        true => Dir::Bi,
                        false => Dir::Uni,
                    })
                    .ok_or(ExaustedStreamsError)?;

                Ok(Stream::new(QuicStreamId::Stream(stream_id)))
            }
        }
    }

    fn write(&mut self, stream: &Stream, data: Bytes, block: bool) -> Result<usize> {
        Ok(match stream.as_stream::<QuicStreamId>()? {
            QuicStreamId::Datagrams => {
                let len = data.len();

                match self.connection.connection.datagrams().send(data, !block) {
                    Ok(()) => len,
                    Err(SendDatagramError::Blocked(_)) => 0,
                    Err(err) => return Err(err.into()),
                }
            }
            &QuicStreamId::Stream(stream_id) => self
                .connection
                .connection
                .send_stream(stream_id)
                .write(data.as_ref())?,
            QuicStreamId::ReceivedDatagram(_) => 0,
        })
    }

    fn read(&mut self, stream: &Stream) -> Result<Result<Bytes, StreamReadError>> {
        match stream.as_stream::<QuicStreamId>()? {
            QuicStreamId::Datagrams => return Err(SendDatagramReadError.into()),
            &QuicStreamId::Stream(stream_id) => {
                let mut stream = self.connection.connection.recv_stream(stream_id);
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
            QuicStreamId::ReceivedDatagram(index) => {
                let Some(index) = index.checked_sub(self.connection.datagrams_queue_offset) else {
                    return Ok(Err(StreamReadError::Closed));
                };

                let Some(datagram) = self.connection.datagrams.get_mut(index) else {
                    return Err(
                        "This datagram stream id was constructed with a higher index than should exist.".into()
                    );
                };

                let result = datagram.take().ok_or(StreamReadError::Closed);

                while let Some(None) = self.connection.datagrams.front() {
                    self.connection.datagrams.pop_front();
                    self.connection.datagrams_queue_offset += 1;
                }

                Ok(result)
            }
        }
    }

    fn close_send_stream(&mut self, stream: &Stream, graceful: bool) -> Result {
        match stream.as_stream::<QuicStreamId>()? {
            QuicStreamId::Datagrams => (),
            &QuicStreamId::Stream(stream_id) => {
                let mut stream = self.connection.connection.send_stream(stream_id);

                match graceful {
                    true => {
                        if let Err(err) = stream.finish() {
                            warn!("Failed to finish quic stream {}: {}", stream_id, err);
                        }
                    }
                    false => {
                        if let Err(err) = stream.reset(VarInt::from_u32(0)) {
                            warn!("Failed to reset quic stream {}: {}", stream_id, err);
                        }
                    }
                }
            }
            QuicStreamId::ReceivedDatagram(_) => (),
        };

        Ok(())
    }

    fn close_recv_stream(&mut self, stream: &Stream) -> Result {
        match stream.as_stream::<QuicStreamId>()? {
            QuicStreamId::Datagrams => (),
            &QuicStreamId::Stream(stream_id) => {
                if let Err(err) = self
                    .connection
                    .connection
                    .recv_stream(stream_id)
                    .stop(VarInt::from_u32(0))
                {
                    warn!("Failed to stop quic stream {}: {}", stream_id, err);
                }
            }
            QuicStreamId::ReceivedDatagram(_) => (),
        };

        Ok(())
    }

    fn accept_stream(&mut self) -> Option<(Stream, StreamRequirements)> {
        let (stream, requirements) = 's: {
            if let Some(stream) = self.connection.connection.streams().accept(Dir::Uni) {
                break 's (
                    QuicStreamId::Stream(stream),
                    StreamRequirements::RELIABLE_ORDERED,
                );
            }

            if let Some(stream) = self.connection.connection.streams().accept(Dir::Bi) {
                break 's (
                    QuicStreamId::Stream(stream),
                    StreamRequirements::RELIABLE_ORDERED.with_bidirectional(true),
                );
            }

            if let Some(datagram) = self.connection.connection.datagrams().recv() {
                let index =
                    self.connection.datagrams_queue_offset + self.connection.datagrams.len();

                self.connection.datagrams.push_back(Some(datagram));

                break 's (
                    QuicStreamId::ReceivedDatagram(index),
                    StreamRequirements::UNRELIABLE,
                );
            }

            return None;
        };

        Some((Stream::new(stream), requirements))
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
#[error("Quic streams in the desired direction are exhausted.")]
pub struct ExaustedStreamsError;

#[derive(Error, Debug)]
#[error("This quic datagram stream is only for sending.")]
pub struct SendDatagramReadError;
