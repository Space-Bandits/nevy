//! Stream headers, which let independent protocols share the streams of one connection.
//!
//! A stream opened with [`new_headered_stream`] starts with its [`StreamHeader`] encoded as a
//! [`VarInt`]. On a stream that is neither reliable nor ordered, each chunk carries its own header,
//! because such a transport can deliver every chunk as a separate stream.
//!
//! [`StreamHeadersPlugin`] takes over accepting streams on every connection, reads each stream's
//! header and queues the stream on that connection's [`HeaderedStreams`] for whichever plugin
//! registered the header with [`RegisterStreamHeader::register_stream_header`]. Streams with an
//! unregistered header are stopped. No other system may call
//! [`accept_stream`](crate::ConnectionContext::accept_stream) while this plugin is added.
//!
//! Header ids are chosen by the application. Plugins that consume streams take their header as a
//! constructor argument rather than defining one.
//!
//! [`new_headered_stream`]: crate::ConnectionContext::new_headered_stream

use std::collections::VecDeque;

use bevy::{
    ecs::{intern::Interned, schedule::ScheduleLabel},
    platform::collections::{HashMap, HashSet},
    prelude::*,
};
use bytes::Bytes;
use log::warn;

use crate::{
    ConnectionOf, DEFAULT_TRANSPORT_SCHEDULE, Endpoint, Stream, StreamReadError,
    StreamRequirements, TransportUpdateSystems,
    varint::{EncodedVarInt, VarInt, VarIntDecoder},
};

/// Identifies which protocol a stream belongs to.
///
/// Applications usually define an enum of their protocols and implement `Into<StreamHeader>` for
/// it. Ids must be at most `u64::MAX >> 7`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StreamHeader(pub u64);

impl StreamHeader {
    /// Returns the bytes that identify this header on the wire.
    pub fn encode(self) -> Result<EncodedVarInt> {
        Ok(VarInt::from_u64(self.0)
            .ok_or("Stream header id was too big for VarInt")?
            .encode())
    }

    /// Returns `true` if every chunk written to a stream with these requirements needs its own
    /// header, and `false` if the header is written once at the start of the stream.
    pub fn per_chunk(requirements: StreamRequirements) -> bool {
        !requirements.reliable && !requirements.ordered
    }
}

/// The system set that accepts streams and reads their headers.
///
/// It runs after [`TransportUpdateSystems`]. Systems that take streams from [`HeaderedStreams`]
/// should run after it.
#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone, Copy)]
pub struct StreamHeaderSystems;

/// Accepts streams on every connection and sorts them by their [`StreamHeader`].
///
/// See the [module docs](self).
pub struct StreamHeadersPlugin {
    schedule: Interned<dyn ScheduleLabel>,
}

impl StreamHeadersPlugin {
    pub fn new(schedule: impl ScheduleLabel) -> Self {
        StreamHeadersPlugin {
            schedule: schedule.intern(),
        }
    }
}

impl Default for StreamHeadersPlugin {
    fn default() -> Self {
        Self::new(DEFAULT_TRANSPORT_SCHEDULE)
    }
}

impl Plugin for StreamHeadersPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RegisteredStreamHeaders>();
        app.register_required_components::<ConnectionOf, HeaderedStreams>();

        app.configure_sets(
            self.schedule,
            StreamHeaderSystems.after(TransportUpdateSystems),
        );

        app.add_systems(
            self.schedule,
            (accept_streams, read_headers)
                .chain()
                .in_set(StreamHeaderSystems),
        );
    }
}

/// The headers whose streams are kept by [`StreamHeadersPlugin`].
#[derive(Resource, Default)]
pub struct RegisteredStreamHeaders(HashSet<StreamHeader>);

impl RegisteredStreamHeaders {
    pub fn contains(&self, header: impl Into<StreamHeader>) -> bool {
        self.0.contains(&header.into())
    }
}

pub trait RegisterStreamHeader {
    /// Marks streams with this header as wanted, so [`StreamHeadersPlugin`] queues them on
    /// [`HeaderedStreams`] instead of stopping them.
    ///
    /// Registering the same header twice will panic. Two different plugins/crates should
    /// never attempt to share a stream header.
    fn register_stream_header(&mut self, header: impl Into<StreamHeader>) -> &mut Self;
}

impl RegisterStreamHeader for App {
    fn register_stream_header(&mut self, header: impl Into<StreamHeader>) -> &mut Self {
        let header = header.into();

        let mut registered = self
            .world_mut()
            .get_resource_or_init::<RegisteredStreamHeaders>();

        if !registered.0.insert(header) {
            panic!("Stream header {:?} was registered twice", header);
        }

        self
    }
}

/// A stream whose header has been read.
pub struct HeaderedStream {
    pub stream: Stream,
    /// The requirements the stream is guaranteed to meet.
    pub requirements: StreamRequirements,
    /// Data that arrived after the header in the same chunk.
    /// It must be processed before reading the stream again.
    pub leftover: Bytes,
}

/// The accepted streams of a connection, sorted by their [`StreamHeader`].
///
/// Required by [`ConnectionOf`] when [`StreamHeadersPlugin`] is added.
#[derive(Component, Default)]
pub struct HeaderedStreams {
    pending: Vec<(Stream, StreamRequirements, VarIntDecoder)>,
    ready: HashMap<StreamHeader, VecDeque<HeaderedStream>>,
}

impl HeaderedStreams {
    /// Takes the oldest accepted stream with this header.
    pub fn take(&mut self, header: impl Into<StreamHeader>) -> Option<HeaderedStream> {
        self.ready.get_mut(&header.into())?.pop_front()
    }
}

fn accept_streams(
    mut connection_q: Query<(Entity, &ConnectionOf, &mut HeaderedStreams)>,
    mut endpoint_q: Query<&mut Endpoint>,
) -> Result {
    for (connection_entity, &ConnectionOf(endpoint_entity), mut streams) in &mut connection_q {
        let mut endpoint = endpoint_q.get_mut(endpoint_entity)?;
        let Ok(mut connection) = endpoint.get_connection(connection_entity) else {
            continue;
        };

        while let Some((stream, requirements)) = connection.accept_stream() {
            streams
                .pending
                .push((stream, requirements, VarIntDecoder::default()));
        }
    }

    Ok(())
}

fn read_headers(
    mut connection_q: Query<(Entity, &ConnectionOf, &mut HeaderedStreams)>,
    mut endpoint_q: Query<&mut Endpoint>,
    registered: Res<RegisteredStreamHeaders>,
) -> Result {
    for (connection_entity, &ConnectionOf(endpoint_entity), mut streams) in &mut connection_q {
        if streams.pending.is_empty() {
            continue;
        }

        let mut endpoint = endpoint_q.get_mut(endpoint_entity)?;
        let Ok(mut connection) = endpoint.get_connection(connection_entity) else {
            continue;
        };

        let streams = streams.as_mut();
        let mut still_pending = Vec::with_capacity(streams.pending.len());

        for (stream, requirements, mut decoder) in streams.pending.drain(..) {
            let header = loop {
                let chunk = match connection.read(&stream)? {
                    Ok(chunk) => chunk,
                    Err(StreamReadError::Blocked) => break None,
                    Err(StreamReadError::Closed) => break Some(None),
                };

                let (id, used) = decoder.push_bytes(chunk.as_ref());

                if let Some(id) = id {
                    break Some(Some((StreamHeader(id.as_u64()), chunk.slice(used..))));
                }
            };

            match header {
                // Still waiting for the rest of the header.
                None => still_pending.push((stream, requirements, decoder)),
                // Closed before a whole header arrived.
                Some(None) => (),
                Some(Some((header, leftover))) => {
                    if !registered.0.contains(&header) {
                        warn!(
                            "Connection {} opened a stream with unregistered header {:?}",
                            connection_entity, header
                        );

                        connection.close_recv_stream(&stream)?;
                        continue;
                    }

                    streams
                        .ready
                        .entry(header)
                        .or_default()
                        .push_back(HeaderedStream {
                            stream,
                            requirements,
                            leftover,
                        });
                }
            }
        }

        streams.pending = still_pending;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip() {
        for id in [0, 1, 127, 128, 300, u64::MAX >> 7] {
            let bytes = StreamHeader(id).encode().unwrap();

            let mut decoder = VarIntDecoder::default();
            let (decoded, used) = decoder.push_bytes(&bytes);

            assert_eq!(decoded.map(VarInt::as_u64), Some(id));
            assert_eq!(used, bytes.len());
        }
    }

    #[test]
    fn header_too_big() {
        assert!(StreamHeader(u64::MAX).encode().is_err());
    }

    #[test]
    fn per_chunk_only_for_unreliable_unordered() {
        assert!(StreamHeader::per_chunk(StreamRequirements::UNRELIABLE));
        assert!(!StreamHeader::per_chunk(StreamRequirements::RELIABLE));
        assert!(!StreamHeader::per_chunk(
            StreamRequirements::RELIABLE_ORDERED
        ));
        assert!(!StreamHeader::per_chunk(
            StreamRequirements::UNRELIABLE.with_ordered(true)
        ));
    }
}
