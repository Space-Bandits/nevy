use bevy::{
    ecs::{intern::Interned, schedule::ScheduleLabel},
    platform::collections::HashMap,
    prelude::*,
};

use crate::{
    ConnectionOf, ConnectionState, Dir, QuicConnection, QuicEndpoint, StreamId, StreamReadError,
    StreamWriteError, UpdateEndpoints,
};

/// System set where streams are accepted and headers are processed.
///
/// Happens after [UpdateEndpoints](crate::UpdateEndpoints)
#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone, Copy)]
pub struct UpdateHeaders;

/// Adds stream header logic to an app.
///
/// The default schedule for updates is `PostUpdate`.
pub struct NevyHeaderPlugin {
    schedule: Interned<dyn ScheduleLabel>,
}

impl NevyHeaderPlugin {
    /// Creates a new plugin with updates happening in a specified schedule.
    pub fn new(schedule: impl ScheduleLabel) -> Self {
        NevyHeaderPlugin {
            schedule: schedule.intern(),
        }
    }
}

impl Default for NevyHeaderPlugin {
    fn default() -> Self {
        Self::new(PostUpdate)
    }
}

impl Plugin for NevyHeaderPlugin {
    fn build(&self, app: &mut App) {
        app.configure_sets(self.schedule, UpdateHeaders.after(UpdateEndpoints));

        app.add_systems(
            self.schedule,
            (insert_stream_header_buffers, read_stream_headers)
                .chain()
                .in_set(UpdateHeaders),
        );
    }
}

#[derive(Default)]
pub struct U16Reader {
    buffer: Vec<u8>,
}

impl U16Reader {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bytes_needed(&self) -> usize {
        2 - self.buffer.len()
    }

    pub fn write(&mut self, mut data: &[u8]) {
        if data.len() > self.bytes_needed() {
            data = &data[..self.bytes_needed()];
        }

        self.buffer.extend_from_slice(data);
    }

    pub fn finish(&self) -> Option<u16> {
        let Ok(&buffer) = self.buffer.as_slice().try_into() else {
            return None;
        };

        Some(u16::from_be_bytes(buffer))
    }
}

/// When this component exists on an endpoint the [RecvStreamHeaders] component will
/// automatically be inserted onto all new connections of that endpoint.
#[derive(Component)]
pub struct EndpointWithHeaderedConnections;

/// Stores incoming streams that are processing headers or ready to be used.
///
/// Insert this component onto a connection to enable stream header processing.
/// Typically this would be done with the [EndpointWithHeaderedConnections] component.
///
/// Use `take_stream` to retrieve a stream that has received a particular header.
#[derive(Component, Default)]
pub struct RecvStreamHeaders {
    buffers: HashMap<StreamId, RecvStreamHeaderState>,
}

enum RecvStreamHeaderState {
    Reading { dir: Dir, buffer: U16Reader },
    HeaderReceived { dir: Dir, header: u16 },
}

pub(crate) fn insert_stream_header_buffers(
    mut commands: Commands,
    connection_q: Query<(Entity, &ConnectionOf), Added<ConnectionOf>>,
    headerd_endpoint_q: Query<(), With<EndpointWithHeaderedConnections>>,
) {
    for (connection_entity, connection_of) in &connection_q {
        if headerd_endpoint_q.contains(**connection_of) {
            commands
                .entity(connection_entity)
                .insert(RecvStreamHeaders::default());
        }
    }
}

pub(crate) fn read_stream_headers(
    mut connection_q: Query<(
        Entity,
        &ConnectionOf,
        &QuicConnection,
        &mut RecvStreamHeaders,
    )>,
    mut endpoint_q: Query<&mut QuicEndpoint>,
) -> Result {
    for (connection_entity, connection_of, quic_connection, mut buffers) in &mut connection_q {
        let mut endpoint = endpoint_q.get_mut(**connection_of)?;

        let connection = endpoint.get_connection(quic_connection)?;

        for dir in [Dir::Uni, Dir::Bi] {
            while let Some(stream_id) = connection.accept_stream(dir) {
                buffers.buffers.insert(
                    stream_id,
                    RecvStreamHeaderState::Reading {
                        dir,
                        buffer: U16Reader::new(),
                    },
                );
            }
        }

        let mut finished_streams = Vec::new();

        for (&stream_id, state) in buffers.buffers.iter_mut() {
            let RecvStreamHeaderState::Reading { dir, buffer } = state else {
                continue;
            };

            loop {
                match connection.read_recv_stream(stream_id, buffer.bytes_needed(), true) {
                    Ok(Some(chunk)) => {
                        buffer.write(&chunk.data);

                        let Some(header) = buffer.finish() else {
                            continue;
                        };

                        let dir = *dir;

                        *state = RecvStreamHeaderState::HeaderReceived { dir, header };

                        break;
                    }
                    Ok(None) => {
                        warn!(
                            "A stream on connection {} finished a stream before sending a header",
                            connection_entity
                        );

                        finished_streams.push(stream_id);
                    }
                    Err(StreamReadError::Blocked) => break,
                    Err(StreamReadError::Reset(code)) => {
                        warn!(
                            "A stream on connection {} was reset with code {} before sending a header",
                            connection_entity, code,
                        );

                        finished_streams.push(stream_id);
                    }
                    Err(err) => {
                        return Err(err.into());
                    }
                }
            }
        }

        for stream_id in finished_streams {
            buffers.buffers.remove(&stream_id);
        }
    }

    Ok(())
}

impl RecvStreamHeaders {
    pub fn take_stream(&mut self, header: impl Into<u16>) -> Option<(StreamId, Dir)> {
        let target_header = header.into();

        if let Some((stream_id, dir)) = self.buffers.iter().find_map(|(&stream_id, state)| {
            let RecvStreamHeaderState::HeaderReceived { dir, header } = state else {
                return None;
            };

            if *header != target_header {
                return None;
            }

            Some((stream_id, *dir))
        }) {
            self.buffers.remove(&stream_id);
            Some((stream_id, dir))
        } else {
            None
        }
    }
}

/// Used for writing a header to a stream before writing data.
pub struct HeaderedStreamState {
    stream_id: StreamId,
    header_buffer: Option<Vec<u8>>,
}

impl HeaderedStreamState {
    pub fn new(stream_id: StreamId, header: impl Into<u16>) -> Self {
        Self {
            stream_id,
            header_buffer: Some(header.into().to_be_bytes().into()),
        }
    }

    /// Gets the stream id.
    pub fn stream_id(&self) -> StreamId {
        self.stream_id
    }

    /// Attempts to write some data to the stream.
    ///
    /// Will only write data once the header has been written.
    pub fn write(
        &mut self,
        connection: &mut ConnectionState,
        data: &[u8],
    ) -> Result<usize, StreamWriteError> {
        if let Some(buffer) = &mut self.header_buffer {
            let bytes_written = connection.write_send_stream(self.stream_id, buffer)?;

            buffer.drain(..bytes_written);

            if buffer.is_empty() {
                self.header_buffer = None;
            } else {
                return Ok(0);
            }
        }

        connection.write_send_stream(self.stream_id, data)
    }
}
