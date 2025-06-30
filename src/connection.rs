use std::collections::VecDeque;

use bevy::prelude::*;
use quinn_proto::VarInt;
use thiserror::Error;

use crate::Direction;

/// The state for a connection accessed through a [QuicEndpoint](crate::endpoint::QuicEndpoint) using a [QuicConnection](crate::connection::QuicConnection) component.
pub struct ConnectionState {
    pub(crate) connection_entity: Entity,
    pub(crate) connection: quinn_proto::Connection,
    pub(crate) stream_events: VecDeque<StreamEvent>,
    // signal to close the connection on the next update
    pub(crate) close: Option<(VarInt, Box<[u8]>)>,
}

/// Id for a stream on a particular connection
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct StreamId(quinn_proto::StreamId);

impl ConnectionState {
    /// Takes the next stream event from the queue.
    ///
    /// If this function is not called regularly the queue will grow indefinitely.
    pub fn poll_stream_event(&mut self) -> Option<StreamEvent> {
        self.stream_events.pop_front()
    }

    /// Accepts a new stream of a certain direction if one is available.
    pub fn accept_stream(&mut self, direction: Direction) -> Option<StreamId> {
        self.connection
            .streams()
            .accept(direction.into())
            .map(|stream_id| StreamId(stream_id))
    }

    /// Attempts to open a new stream of a certain direction.
    ///
    /// Fails if the maximum number of these streams has been reached.
    pub fn open_stream(&mut self, direction: Direction) -> Option<StreamId> {
        self.connection
            .streams()
            .open(direction.into())
            .map(|stream_id| StreamId(stream_id))
    }

    /// Returns the number of send streams that may have unacknowledged data.
    ///
    /// Can be used to determine if there is outstanding data that has not been transmitted.
    pub fn get_open_send_streams(&mut self) -> usize {
        self.connection.streams().send_streams()
    }

    /// Gets the number of remotely opened streams of a certain direction.
    pub fn get_open_remote_streams(&mut self, direction: Direction) -> u64 {
        self.connection
            .streams()
            .remote_open_streams(direction.into())
    }

    /// Sets the maximum number of concurrent streams that the peer can open in a certain direction.
    pub fn set_max_concurrent_streams(
        &mut self,
        direction: Direction,
        count: u64,
    ) -> Result<(), VarIntBoundsExceeded> {
        self.connection.set_max_concurrent_streams(
            direction.into(),
            VarInt::from_u64(count)
                .map_err(|quinn_proto::VarIntBoundsExceeded| VarIntBoundsExceeded)?,
        );

        Ok(())
    }

    /// Sets the priority of a send stream.
    pub fn set_send_stream_priority(
        &mut self,
        StreamId(stream_id): StreamId,
        priority: i32,
    ) -> Result<(), ClosedStreamError> {
        self.connection
            .send_stream(stream_id)
            .set_priority(priority)
            .map_err(|quinn_proto::ClosedStream { .. }| ClosedStreamError)
    }

    /// Gets the priority of a send stream.
    pub fn get_send_stream_priority(
        &mut self,
        StreamId(stream_id): StreamId,
    ) -> Result<i32, ClosedStreamError> {
        self.connection
            .send_stream(stream_id)
            .priority()
            .map_err(|quinn_proto::ClosedStream { .. }| ClosedStreamError)
    }

    /// Attempts to write some data to a stream.
    ///
    /// If successful returns the number of bytes that were successfully written.
    pub fn write_send_stream(
        &mut self,
        StreamId(stream_id): StreamId,
        data: &[u8],
    ) -> Result<usize, StreamWriteError> {
        let mut stream = self.connection.send_stream(stream_id);

        match stream.write(data) {
            Ok(size) => Ok(size),
            Err(quinn_proto::WriteError::Blocked) => Ok(0),
            Err(quinn_proto::WriteError::ClosedStream) => {
                Err(StreamWriteError::ClosedStream(ClosedStreamError))
            }
            Err(quinn_proto::WriteError::Stopped(code)) => {
                Err(StreamWriteError::Stopped(code.into_inner()))
            }
        }
    }

    /// Attempts to read up to `max_size` bytes from a stream.
    ///
    /// When none is returned the stream has been finished.
    /// If the stream is blocked on waiting for more data [StreamReadError::Blocked] will be returned.
    ///
    /// If an out of order read is attempted, only out of order reads are valid from that point on.
    pub fn read_recv_stream(
        &mut self,
        StreamId(stream_id): StreamId,
        max_size: usize,
        ordered: bool,
    ) -> Result<Option<Chunk>, StreamReadError> {
        let mut stream = self.connection.recv_stream(stream_id);

        let mut chunks = match stream.read(ordered) {
            Ok(chunks) => chunks,
            Err(quinn_proto::ReadableError::ClosedStream) => {
                return Err(StreamReadError::ClosedStream(ClosedStreamError))
            }
            Err(quinn_proto::ReadableError::IllegalOrderedRead) => {
                return Err(StreamReadError::IllegalOrderedRead)
            }
        };

        let chunk = match chunks.next(max_size) {
            Ok(Some(chunk)) => chunk,
            Ok(None) => return Ok(None),
            Err(quinn_proto::ReadError::Blocked) => return Err(StreamReadError::Blocked),
            Err(quinn_proto::ReadError::Reset(code)) => {
                return Err(StreamReadError::Reset(code.into_inner()));
            }
        };

        // packet will be transmitted on the next update
        let _ = chunks.finalize();

        Ok(Some(Chunk {
            offset: chunk.offset,
            data: Vec::from(chunk.bytes).into(),
        }))
    }

    /// Finishes a send stream
    pub fn finish_send_stream(
        &mut self,
        StreamId(stream_id): StreamId,
    ) -> Result<(), StreamFinishError> {
        match self.connection.send_stream(stream_id).finish() {
            Ok(()) => Ok(()),
            Err(quinn_proto::FinishError::Stopped(code)) => {
                Err(StreamFinishError::Stopped(code.into_inner()))
            }
            Err(quinn_proto::FinishError::ClosedStream) => {
                Err(StreamFinishError::ClosedStream(ClosedStreamError))
            }
        }
    }

    /// Abandon transmitting data on a send stream
    pub fn reset_send_stream(
        &mut self,
        StreamId(stream_id): StreamId,
        code: u64,
    ) -> Result<(), ResetStreamError> {
        match self
            .connection
            .send_stream(stream_id)
            .reset(
                VarInt::from_u64(code).map_err(|quinn_proto::VarIntBoundsExceeded| {
                    ResetStreamError::InvalidCode(VarIntBoundsExceeded)
                })?,
            ) {
            Ok(()) => Ok(()),
            Err(quinn_proto::ClosedStream { .. }) => {
                Err(ResetStreamError::ClosedStream(ClosedStreamError))
            }
        }
    }

    /// Will tell the peer to stop sending data on a recv stream
    pub fn stop_recv_stream(
        &mut self,
        StreamId(stream_id): StreamId,
        code: u64,
    ) -> Result<(), StopStreamError> {
        match self
            .connection
            .recv_stream(stream_id)
            .stop(
                VarInt::from_u64(code).map_err(|quinn_proto::VarIntBoundsExceeded| {
                    StopStreamError::InvalidCode(VarIntBoundsExceeded)
                })?,
            ) {
            Ok(()) => Ok(()),
            Err(quinn_proto::ClosedStream { .. }) => {
                Err(StopStreamError::ClosedStream(ClosedStreamError))
            }
        }
    }

    /// Checks if a send stream has been stopped by the peer, returns the code if it was.
    pub fn send_stream_stopped(
        &mut self,
        StreamId(stream_id): StreamId,
    ) -> Result<Option<u64>, ClosedStreamError> {
        self.connection
            .send_stream(stream_id)
            .stopped()
            .map(|code| code.map(|map| map.into_inner()))
            .map_err(|quinn_proto::ClosedStream { .. }| ClosedStreamError)
    }

    /// Closes the connection in the next update
    ///
    /// Will do nothing if the stream has already been closed.
    pub fn close(&mut self, code: u64, reason: Box<[u8]>) -> Result<(), VarIntBoundsExceeded> {
        let Ok(code) = VarInt::from_u64(code) else {
            return Err(VarIntBoundsExceeded);
        };

        if self.close.is_some() || self.connection.is_closed() {
            return Ok(());
        }

        self.close = Some((code, reason));

        Ok(())
    }

    /// Gets the maximum size datagram that can be sent.
    pub fn get_datagram_max_size(&mut self) -> Option<usize> {
        self.connection.datagrams().max_size()
    }

    /// Gets the amount of space available in the datagram send buffer.
    ///
    /// When greater than zero, sending a datagram of at most this size is guaranteed not to cause older datagrams to be dropped.
    pub fn get_datagram_send_buffer_space(&mut self) -> usize {
        self.connection.datagrams().send_buffer_space()
    }

    /// Attempts to write an unreliable unordered datagram to an internal buffer.
    ///
    /// If `drop_old` is true the oldest datagram in the buffer will be dropped if more space is needed.
    ///
    /// If `drop_old` is false and the buffer is full [SendDatagramError::Blocked] will be returned.
    pub fn send_datagram(
        &mut self,
        data: Box<[u8]>,
        drop_old: bool,
    ) -> Result<(), SendDatagramError> {
        match self.connection.datagrams().send(data.into(), drop_old) {
            Ok(()) => Ok(()),
            Err(quinn_proto::SendDatagramError::Disabled) => Err(SendDatagramError::Disabled),
            Err(quinn_proto::SendDatagramError::TooLarge) => Err(SendDatagramError::TooLarge),
            Err(quinn_proto::SendDatagramError::UnsupportedByPeer) => {
                Err(SendDatagramError::UnsupportedByPeer)
            }
            Err(quinn_proto::SendDatagramError::Blocked(data)) => {
                Err(SendDatagramError::Blocked(Vec::from(data).into()))
            }
        }
    }

    /// Take the next unordered unreliable datagram from the receive buffer.
    pub fn receive_datagram(&mut self) -> Option<Box<[u8]>> {
        self.connection
            .datagrams()
            .recv()
            .map(|data| Vec::from(data).into())
    }

    pub fn accept_uni_stream(&self) -> Option<StreamId> {
        todo!()
    }
}

/// Events to do with streams.
///
/// Represents a [quinn_proto::StreamEvent]
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// One or more new streams has been opened and might be readable
    Opened {
        /// Directionality for which streams have been opened
        direction: Direction,
    },
    /// A currently open stream likely has data or errors waiting to be read
    Readable {
        /// Which stream is now readable
        id: StreamId,
    },
    /// A formerly write-blocked stream might be ready for a write or have been stopped
    ///
    /// Only generated for streams that are currently open.
    Writable {
        /// Which stream is now writable
        id: StreamId,
    },
    /// A finished stream has been fully acknowledged or stopped
    Finished {
        /// Which stream has been finished
        id: StreamId,
    },
    /// The peer asked us to stop sending on an outgoing stream
    Stopped {
        /// Which stream has been stopped
        id: StreamId,
        /// Error code supplied by the peer
        error_code: u64,
    },
    /// At least one new stream of a certain directionality may be opened
    Available {
        /// Directionality for which streams are newly available
        direction: Direction,
    },
}

impl From<quinn_proto::StreamEvent> for StreamEvent {
    fn from(value: quinn_proto::StreamEvent) -> Self {
        match value {
            quinn_proto::StreamEvent::Opened { dir } => StreamEvent::Opened {
                direction: dir.into(),
            },
            quinn_proto::StreamEvent::Readable { id } => StreamEvent::Readable { id: StreamId(id) },
            quinn_proto::StreamEvent::Writable { id } => StreamEvent::Writable { id: StreamId(id) },
            quinn_proto::StreamEvent::Finished { id } => StreamEvent::Finished { id: StreamId(id) },
            quinn_proto::StreamEvent::Stopped { id, error_code } => StreamEvent::Stopped {
                id: StreamId(id),
                error_code: error_code.into_inner(),
            },
            quinn_proto::StreamEvent::Available { dir } => StreamEvent::Available {
                direction: dir.into(),
            },
        }
    }
}

/// A chunk of data received by a stream
pub struct Chunk {
    /// The offset of the chunk in the stream for out of order reads
    pub offset: u64,
    pub data: Box<[u8]>,
}

/// A value was too large for a quic variable length integer
///
/// Must be less than 2^62.
#[derive(Clone, Copy, Debug, Error)]
#[error("Value was too large for a quic variable length integer")]
pub struct VarIntBoundsExceeded;

#[derive(Clone, Copy, Debug, Error)]
#[error("This stream has been closed and does not exist")]
pub struct ClosedStreamError;

/// An error returned when reading from a stream fails
#[derive(Debug, Clone, Error)]
pub enum StreamReadError {
    /// The stream is blocked on receiving more data
    #[error("No more data available yet")]
    Blocked,
    /// The stream has been closed and no longer exists
    #[error(transparent)]
    ClosedStream(#[from] ClosedStreamError),
    /// An unordered read was previously performed on this stream, no more ordered reads are allowed
    #[error("Illegal ordered read attempted after unordered read")]
    IllegalOrderedRead,
    /// The stream was reset by the peer with an application defined error code
    #[error("Stream reset by peer with code {0}")]
    Reset(u64),
}

/// An error returned when writing to a stream fails
#[derive(Debug, Clone, Error)]
pub enum StreamWriteError {
    /// The stream has been closed and no longer exists
    #[error(transparent)]
    ClosedStream(#[from] ClosedStreamError),
    /// The peer stopped the stream with an application defined error code
    #[error("Stream stopped by peer with code {0}")]
    Stopped(u64),
}

/// An error returned when finishing a stream fails
#[derive(Debug, Clone, Error)]
pub enum StreamFinishError {
    /// The stream has been closed and no longer exists
    #[error(transparent)]
    ClosedStream(#[from] ClosedStreamError),
    /// The peer stopped the stream with an application defined error code
    #[error("Stream stopped by peer with code {0}")]
    Stopped(u64),
}

/// An error returned when resetting a stream fails
#[derive(Debug, Clone, Error)]
pub enum ResetStreamError {
    #[error("Error code too large")]
    InvalidCode(#[from] VarIntBoundsExceeded),
    /// The stream has been closed and no longer exists
    #[error(transparent)]
    ClosedStream(#[from] ClosedStreamError),
}

/// An error returned when stopping a stream fails
#[derive(Debug, Clone, Error)]
pub enum StopStreamError {
    #[error("Error code too large")]
    InvalidCode(#[from] VarIntBoundsExceeded),
    /// The stream has been closed and no longer exists
    #[error(transparent)]
    ClosedStream(#[from] ClosedStreamError),
}

/// An error returned when sending a datagram fails
#[derive(Debug, Clone, Error)]
pub enum SendDatagramError {
    /// Datagrams have been disabled locally
    #[error("Datagrams disabled locally")]
    Disabled,
    /// Datagram and overhead are too large for the current mtu
    #[error("Datagram too large")]
    TooLarge,
    /// Datagrams are not supported by peer
    #[error("Datagrams are not supported by peer")]
    UnsupportedByPeer,
    /// Send buffer has been filled, can't send more datagrams without dropping old ones.
    #[error("Datagram blocked")]
    Blocked(Box<[u8]>),
}
