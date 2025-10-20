//! This module is an optional feature used for sending typed messages over a connection.
//!
//! NetMessages operates on top of stream headers on a particular stream id.

use std::{collections::VecDeque, marker::PhantomData};

use bevy::{
    ecs::{component::Mutable, intern::Interned, schedule::ScheduleLabel},
    platform::collections::HashMap,
    prelude::*,
};
use log::warn;
use quinn_proto::Dir;
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    ConnectionOf, QuicConnection, QuicEndpoint, RecvStreamHeaders, StreamId, StreamReadError,
    UpdateEndpointSystems, UpdateHeaderSystems, headers::U16Reader,
};
use senders::NetMessageSendHeader;

pub mod senders;

pub(crate) fn bincode_config() -> bincode::config::Configuration {
    bincode::config::standard()
}

/// System sets messages are processed.
///
/// Happens after [UpdateHeaderSystems](crate::headers::UpdateHeaderSystems)
#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone, Copy)]
pub enum UpdateNetMessageSystems {
    InsertComponents,
    ReadNetMessages,
    DeserializeNetMessages,
}

/// Adds message receiving logic to an app.
///
/// This should be
pub struct NevyNetMessagesPlugin {
    schedule: Interned<dyn ScheduleLabel>,
    message_header: u16,
}

impl NevyNetMessagesPlugin {
    /// Constructs the plugin. You must provide which stream header to receive messages on.
    ///
    /// The default schedule for updates is `PostUpdate`, use [Self::new_with_schedule] to change this.
    pub fn new(message_header: impl Into<u16>) -> Self {
        NevyNetMessagesPlugin {
            schedule: PostUpdate.intern(),
            message_header: message_header.into(),
        }
    }

    /// Creates a new plugin with updates happening in a specified schedule.
    pub fn new_with_schedule(message_header: impl Into<u16>, schedule: impl ScheduleLabel) -> Self {
        NevyNetMessagesPlugin {
            schedule: schedule.intern(),
            message_header: message_header.into(),
        }
    }
}

impl Plugin for NevyNetMessagesPlugin {
    fn build(&self, app: &mut App) {
        app.configure_sets(
            self.schedule,
            (
                UpdateNetMessageSystems::InsertComponents,
                UpdateNetMessageSystems::ReadNetMessages.after(UpdateHeaderSystems),
                UpdateNetMessageSystems::DeserializeNetMessages,
            )
                .chain()
                .after(UpdateEndpointSystems),
        );

        app.insert_resource(NetMessageReceiveHeader(self.message_header));
        app.insert_resource(NetMessageSendHeader(self.message_header));

        app.add_systems(
            self.schedule,
            (
                insert_recv_stream_buffers.in_set(UpdateNetMessageSystems::InsertComponents),
                (take_message_streams, read_message_streams)
                    .chain()
                    .in_set(UpdateNetMessageSystems::ReadNetMessages),
            ),
        );
    }
}

#[derive(Resource, Default)]
struct NextNetMessageId(u16);

/// Trait extension for [App](crate::App) that allows assigning adding messages.
pub trait AddNetMessage {
    fn add_net_message<T>(&mut self)
    where
        T: Serialize + DeserializeOwned + Send + Sync + 'static;
}

impl AddNetMessage for App {
    /// Assigns a unique id to a message and adds logic for deserializing that type.
    ///
    /// The order that messages are added to an app is what defines the protocol and should be identical for both the server and client.
    /// The best way of accomplishing this would be through a function or plugin that is in a shared dependency both binaries.
    fn add_net_message<T>(&mut self)
    where
        T: Serialize + DeserializeOwned,
        NetMessageId<T>: Resource,
        ReceivedNetMessages<T>: Component<Mutability = Mutable>,
    {
        let mut next_message_id = self.world_mut().get_resource_or_init::<NextNetMessageId>();

        let message_id = next_message_id.0;
        next_message_id.0 += 1;

        self.insert_resource(NetMessageId::<T> {
            _p: PhantomData,
            id: message_id,
        });

        self.add_observer(insert_received_message_buffers::<T>);

        self.add_systems(
            PostUpdate,
            deserialize_messages::<T>.in_set(UpdateNetMessageSystems::DeserializeNetMessages),
        );
    }
}

/// Holds the unique stream header id to recieve messages on.
#[derive(Resource)]
pub struct NetMessageReceiveHeader(pub u16);

/// Holds the id of a message.
///
/// This resource is added to the app by using [`app.add_message::<T>()`](AddNetMessage::add_message)
/// and needs to be provided when sending a message.
#[derive(Resource)]
pub struct NetMessageId<T> {
    _p: PhantomData<T>,
    id: u16,
}

impl<T> Clone for NetMessageId<T> {
    fn clone(&self) -> Self {
        Self {
            _p: PhantomData,
            id: self.id,
        }
    }
}

impl<T> Copy for NetMessageId<T> {}

/// When this component exists on an endpoint the [NetMessageRecvStreams] component will
/// automatically be inserted onto all new connections of that endpoint.
#[derive(Component)]
pub struct EndpointWithNetMessageConnections;

/// Holds the state machines for the message streams of a connection.
///
/// Insert this component onto a connection to enable message receiving.
/// Typically this would be done with the [EndpointWithNetMessageConnections] component.
///
/// Once deserialized the messages will be inserted into their [ReceivedNetMessages<T>] component.
/// These components are automatically inserted when this component is inserted.
#[derive(Component, Default)]
pub struct NetMessageReceiveStreams {
    streams: HashMap<StreamId, NetMessageRecvStreamState>,
    messages: HashMap<u16, VecDeque<Box<[u8]>>>,
}

/// State machine for a single message stream on a connection.
enum NetMessageRecvStreamState {
    ReadingId {
        buffer: U16Reader,
    },
    ReadingLength {
        message_id: u16,
        buffer: U16Reader,
    },
    ReadingNetMessage {
        message_id: u16,
        length: u16,
        buffer: Vec<u8>,
    },
}

/// Buffer of received messages from all streams for a connection.
///
/// If this buffer is not drained it will fill up indefinitely.
#[derive(Component)]
pub struct ReceivedNetMessages<T> {
    messages: VecDeque<T>,
}

fn insert_recv_stream_buffers(
    mut commands: Commands,
    connection_q: Query<(Entity, &ConnectionOf), Added<ConnectionOf>>,
    message_endpoint_q: Query<(), With<EndpointWithNetMessageConnections>>,
) {
    for (connection_entity, connection_of) in &connection_q {
        if message_endpoint_q.contains(**connection_of) {
            commands
                .entity(connection_entity)
                .insert(NetMessageReceiveStreams::default());
        }
    }
}

fn insert_received_message_buffers<T>(
    insert: On<Insert, NetMessageReceiveStreams>,
    mut commands: Commands,
) where
    ReceivedNetMessages<T>: Component,
{
    commands
        .entity(insert.entity)
        .insert(ReceivedNetMessages::<T> {
            messages: VecDeque::new(),
        });
}

fn take_message_streams(
    mut connection_q: Query<(
        Entity,
        &mut RecvStreamHeaders,
        &mut NetMessageReceiveStreams,
    )>,
    message_stream_header: Res<NetMessageReceiveHeader>,
) {
    for (connection_entity, mut headers, mut recv_streams) in &mut connection_q {
        while let Some((stream_id, dir)) = headers.take_stream(message_stream_header.0) {
            if let Dir::Bi = dir {
                warn!(
                    "Connection {} opened a bidirectional message stream which should only ever be unidirectional.",
                    connection_entity
                );
            }

            recv_streams.streams.insert(
                stream_id,
                NetMessageRecvStreamState::ReadingId {
                    buffer: U16Reader::new(),
                },
            );
        }
    }
}

fn read_message_streams(
    mut connection_q: Query<(
        Entity,
        &ConnectionOf,
        &QuicConnection,
        &mut NetMessageReceiveStreams,
    )>,
    mut endpoint_q: Query<&mut QuicEndpoint>,
) -> Result {
    for (connection_entity, connection_of, connection, mut recv_streams) in &mut connection_q {
        let buffers = recv_streams.as_mut();

        let mut endpoint = endpoint_q.get_mut(**connection_of)?;

        let mut connection = endpoint.get_connection(connection)?;

        let mut finished = Vec::new();

        for (&stream_id, stream_state) in buffers.streams.iter_mut() {
            loop {
                let bytes_needed = match stream_state {
                    NetMessageRecvStreamState::ReadingId { buffer } => buffer.bytes_needed(),
                    NetMessageRecvStreamState::ReadingLength { buffer, .. } => {
                        buffer.bytes_needed()
                    }
                    NetMessageRecvStreamState::ReadingNetMessage { buffer, length, .. } => {
                        *length as usize - buffer.len()
                    }
                };

                let chunk = match connection.read_recv_stream(stream_id, bytes_needed, true) {
                    Ok(Some(chunk)) => chunk,
                    Ok(None) => {
                        finished.push(stream_id);

                        break;
                    }
                    Err(StreamReadError::Blocked) => break,
                    Err(StreamReadError::Reset(code)) => {
                        warn!(
                            "A stream on connection {} was reset with code {} before sending a header",
                            connection_entity, code,
                        );

                        finished.push(stream_id);

                        break;
                    }
                    Err(err) => {
                        return Err(err.into());
                    }
                };

                match stream_state {
                    NetMessageRecvStreamState::ReadingId { buffer } => {
                        buffer.write(&chunk.data);

                        let Some(message_id) = buffer.finish() else {
                            continue;
                        };

                        *stream_state = NetMessageRecvStreamState::ReadingLength {
                            message_id,
                            buffer: U16Reader::new(),
                        };
                    }
                    NetMessageRecvStreamState::ReadingLength { message_id, buffer } => {
                        buffer.write(&chunk.data);

                        let Some(length) = buffer.finish() else {
                            continue;
                        };

                        if length == 0 {
                            buffers
                                .messages
                                .entry(*message_id)
                                .or_default()
                                .push_back(default());

                            *stream_state = NetMessageRecvStreamState::ReadingId {
                                buffer: U16Reader::new(),
                            };
                        } else {
                            *stream_state = NetMessageRecvStreamState::ReadingNetMessage {
                                message_id: *message_id,
                                length,
                                buffer: Vec::new(),
                            };
                        }
                    }
                    NetMessageRecvStreamState::ReadingNetMessage {
                        message_id,
                        length,
                        buffer,
                    } => {
                        buffer.extend(chunk.data);

                        if buffer.len() != *length as usize {
                            continue;
                        }

                        let message = std::mem::take(buffer);

                        buffers
                            .messages
                            .entry(*message_id)
                            .or_default()
                            .push_back(message.into_boxed_slice());

                        *stream_state = NetMessageRecvStreamState::ReadingId {
                            buffer: U16Reader::new(),
                        };
                    }
                }
            }
        }

        for stream_id in finished {
            buffers.streams.remove(&stream_id);
        }
    }

    Ok(())
}

fn deserialize_messages<T>(
    message_id: Res<NetMessageId<T>>,
    mut connection_q: Query<(&mut NetMessageReceiveStreams, &mut ReceivedNetMessages<T>)>,
) where
    T: DeserializeOwned,
    NetMessageId<T>: Resource,
    ReceivedNetMessages<T>: Component<Mutability = Mutable>,
{
    for (mut stream_states, mut message_buffer) in connection_q.iter_mut() {
        let Some(serialized_buffer) = stream_states.messages.get_mut(&message_id.id) else {
            continue;
        };

        for bytes in serialized_buffer.drain(..) {
            match bincode::serde::decode_from_slice(&bytes, bincode_config()) {
                Ok((message, _)) => {
                    message_buffer.messages.push_back(message);
                }
                Err(error) => {
                    warn!(
                        "Failed to deserialize \"{}\" message: {}",
                        std::any::type_name::<T>(),
                        error
                    );
                }
            }
        }
    }
}

impl<T> ReceivedNetMessages<T> {
    /// Removes all messages from the buffer and returns an iterator over them.
    pub fn drain(&mut self) -> impl Iterator<Item = T> + '_ {
        self.messages.drain(..)
    }

    /// Removes the next message from the buffer and returns it if it exists.
    pub fn next(&mut self) -> Option<T> {
        self.messages.pop_front()
    }
}
