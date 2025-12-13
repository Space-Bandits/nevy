use std::{collections::VecDeque, net::UdpSocket};

use bevy::prelude::*;
use bytes::Bytes;
use quinn_proto::{Dir, StreamEvent, VarInt};
use quinn_udp::UdpSocketState;

use crate::{
    Connection, ConnectionContext, Stream, StreamId, StreamRequirements,
    protocols::quic::udp_transmit,
};

pub(crate) struct QuicConnectionState {
    pub(crate) connection_entity: Entity,
    pub(crate) connection: quinn_proto::Connection,
    pub(crate) stream_events: VecDeque<StreamEvent>,
    /// signal to close the connection on the next update
    pub(crate) close: Option<(VarInt, Box<[u8]>)>,
}

#[derive(Clone, Copy)]
pub enum QuicStreamId {
    Datagrams,
    Stream(quinn_proto::StreamId),
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
                    .ok_or(NewStreamError::TransportError)?;

                Ok(Stream::new(QuicStreamId::Stream(stream_id)))
            }
        }
    }

    fn write(&mut self, stream: &Stream, data: &[u8], block: bool) -> Result<usize> {
        Ok(match stream.as_stream::<QuicStreamId>()? {
            QuicStreamId::Datagrams => {
                self.connection
                    .connection
                    .datagrams()
                    .send(Bytes::copy_from_slice(data), !block)
                    .map_err(|err| BevyError::from(err))?;

                data.len()
            }
            &QuicStreamId::Stream(stream_id) => self
                .connection
                .connection
                .send_stream(stream_id)
                .write(data)?,
        })
    }

    fn close_send_stream(&mut self, stream: &Stream, graceful: bool) {
        todo!()
    }

    fn close_recv_stream(&mut self, stream: &Stream, graceful: bool) {
        todo!()
    }
}
