use std::{collections::VecDeque, marker::PhantomData};

use bevy::{
    ecs::{entity::EntityHashMap, schedule::ScheduleLabel, system::SystemParam},
    prelude::*,
};
use quinn_proto::Dir;
use serde::Serialize;

use crate::{
    DEFAULT_NEVY_SCHEDULE,
    connection::{ConnectionState, StreamWriteError},
    headers::HeaderedStreamState,
    messages::{
        ConnectionOf, NetMessageId, QuicConnection, QuicEndpoint, StreamId, UpdateEndpoints,
        bincode_config,
    },
};

/// Holds the unique stream header id to send messages on.
///
/// [`LocalNetMessageSender`]s and [`SharedNetMessageSender`]s
/// use this as the default header to open messaging streams with.
#[derive(Resource)]
pub struct NetMessageSendHeader(pub u16);

/// State machine for a stream that sends messages.
///
/// This can be used directly, but it is easier to use either [LocalNetMessageSender] or [SharedNetMessageSender]
pub struct NetMessageSendStreamState {
    stream: HeaderedStreamState,
    buffer: VecDeque<u8>,
}

impl NetMessageSendStreamState {
    /// Creates a new state machine that will send data on a stream.
    ///
    /// The provided stream header should be the unique id that the peer is expecting for messages.
    ///
    /// NetMessage streams should be unidirectional, if they aren't it is your responsibility to handle the receiving direction.
    pub fn new(stream_id: StreamId, header: impl Into<u16>) -> Self {
        Self {
            stream: HeaderedStreamState::new(stream_id, header.into()),
            buffer: VecDeque::new(),
        }
    }

    /// Gets the stream id for the state machine.
    ///
    /// Can be used to modify the stream or finish it.
    pub fn stream_id(&self) -> StreamId {
        self.stream.stream_id()
    }

    /// Returns true if the internal buffer has been entirely written to the connection.
    pub fn uncongested(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Writes as much of the internal buffer as possible to the connection.
    ///
    /// This should be called frequently even if messages aren't being written to make sure that all data is sent.
    pub fn flush(&mut self, connection: &mut ConnectionState) -> Result<(), StreamWriteError> {
        if self.buffer.len() == 0 {
            return Ok(());
        }

        let written_bytes = self
            .stream
            .write(connection, self.buffer.make_contiguous())?;

        self.buffer.drain(..written_bytes);

        Ok(())
    }

    /// Attempts to send a message
    ///
    /// If `queue` is true the message will always be written and `Ok(true)` will be returned.
    /// This will cause the internal buffer to grow without limit if the stream is congested.
    /// See `Self::uncongested`.
    ///
    /// If `queue` is false and the stream is congested the message will not be written and `Ok(false)` will be returned.
    pub fn write<T>(
        &mut self,
        message_id: NetMessageId<T>,
        connection: &mut ConnectionState,
        message: &T,
        queue: bool,
    ) -> Result<bool, StreamWriteError>
    where
        T: Serialize,
    {
        // only attempt to write data if queueing or uncongested
        if !(queue || self.uncongested()) {
            return Ok(false);
        }

        // serialize
        let message_data = match bincode::serde::encode_to_vec(message, bincode_config()) {
            Ok(data) => data,
            Err(err) => panic!("Failed to serialize message: {}", err),
        };

        // write the message id
        self.buffer.extend(message_id.id.to_be_bytes());

        // write the message length
        let message_length: u16 = message_data
            .len()
            .try_into()
            .expect("NetMessage was too long");
        self.buffer.extend(message_length.to_be_bytes());

        // write the message
        self.buffer.extend(message_data);

        self.flush(connection)?;

        Ok(true)
    }
}

/// System parameters needed by a shared or local message sender
#[derive(SystemParam)]
pub struct SenderParams<'w, 's> {
    connection_q: Query<'w, 's, (&'static QuicConnection, &'static ConnectionOf)>,
    endpoint_q: Query<'w, 's, &'static mut QuicEndpoint>,
    stream_header: Option<Res<'w, NetMessageSendHeader>>,
}

/// The state for a local or shared message sender
#[derive(Default)]
pub struct SenderState {
    connections: EntityHashMap<NetMessageSendStreamState>,
}

/// System parameter that holds a [Local] [NetMessageSendStreamState] for each connection.
///
/// Should be used when the ordering of messages sent in different systems isn't important.
///
/// This sender needs to be flushed manually.
/// This is best done by putting a [Self::flush] at the beginning of every system using this parameter.
#[derive(SystemParam)]
pub struct LocalNetMessageSender<'w, 's> {
    params: SenderParams<'w, 's>,
    state: Local<'s, SenderState>,
}

/// The shared state for a [SharedNetMessageSender].
#[derive(Resource)]
struct SharedNetMessageSenderState<S> {
    _p: PhantomData<S>,
    state: SenderState,
}

/// System parameter that accesses a shared [NetMessageSendStreamState] for each connection.
///
/// Should be used when the ordering of messages sent in different systems is important.
///
/// Each shared sender is accessed with a marker type `S` and needs to be added to the app using [AddSharedSender::add_shared_sender].
///
/// This sender is flushed automatically.
#[derive(SystemParam)]
pub struct SharedNetMessageSender<'w, 's, S>
where
    S: Send + Sync + 'static,
{
    params: SenderParams<'w, 's>,
    state: ResMut<'w, SharedNetMessageSenderState<S>>,
}

/// Trait extension for [App](crate::App) that allows adding a [SharedNetMessageSender].
pub trait AddSharedSender {
    /// Adds a [SharedNetMessageSender] to the app that will flush in a certain schedule.
    ///
    /// If added to the same schedule that [NevyPlugin](crate::NevyPlugin) runs in it will flush before [UpdateEndpoints].
    fn add_shared_sender_with_schedule<S>(&mut self, nevy_schedule: impl ScheduleLabel)
    where
        S: Send + Sync + 'static;

    /// Adds a [SharedNetMessageSender] to the app that will flush in the default schedule for nevy.
    fn add_shared_sender<S>(&mut self)
    where
        S: Send + Sync + 'static,
    {
        self.add_shared_sender_with_schedule::<S>(DEFAULT_NEVY_SCHEDULE);
    }
}

impl AddSharedSender for App {
    fn add_shared_sender_with_schedule<S>(&mut self, nevy_schedule: impl ScheduleLabel)
    where
        S: Send + Sync + 'static,
    {
        self.insert_resource(SharedNetMessageSenderState::<S> {
            _p: PhantomData,
            state: SenderState::default(),
        });

        self.add_systems(
            nevy_schedule,
            flush_shared_sender::<S>.before(UpdateEndpoints),
        );
    }
}

fn flush_shared_sender<S>(mut sender: SharedNetMessageSender<S>) -> Result
where
    S: Send + Sync + 'static,
{
    sender.flush()
}

pub trait NetMessageSenderContext<'w, 's> {
    /// Gets the ecs parameters and sender state for the different types of message sender.
    /// This is a helper function for the default implementations of the other methods on this trait.
    fn context(&mut self) -> (&mut SenderParams<'w, 's>, &mut SenderState);
}

pub trait NetMessageSender<'w, 's>: NetMessageSenderContext<'w, 's> {
    /// Drives state machines for all connections to completion.
    ///
    /// Should be called regularly to ensure messages that are still in buffers are sent.
    fn flush(&mut self) -> Result {
        let (params, state) = self.context();

        let mut removed_connections = Vec::new();

        for (&connection_entity, stream_state) in state.connections.iter_mut() {
            let Ok((connection, connection_of)) = params.connection_q.get(connection_entity) else {
                removed_connections.push(connection_entity);

                continue;
            };

            let mut endpoint = params.endpoint_q.get_mut(**connection_of)?;

            let connection = endpoint.get_connection(connection)?;

            stream_state.flush(connection)?;
        }

        for connection_entity in removed_connections {
            state.connections.remove(&connection_entity);
        }

        Ok(())
    }

    /// Writes
    ///
    /// The provided stream header should be the unique id that the peer is expecting.
    /// This method is provided for cases where you may be communicating with multiple clients that are expecting different stream headers for messages.
    /// See [NetMessageStreamHeader](crate::messages::NetMessageReceiveHeader).
    fn write_with_header<T>(
        &mut self,
        header: impl Into<u16>,
        connection_entity: Entity,
        message_id: NetMessageId<T>,
        queue: bool,
        message: &T,
    ) -> Result<bool>
    where
        T: Serialize,
    {
        let (params, state) = self.context();

        let (connection, connection_of) = params.connection_q.get(connection_entity)?;

        let mut endpoint = params.endpoint_q.get_mut(**connection_of)?;

        let connection = endpoint.get_connection(connection)?;

        let state = match state.connections.entry(connection_entity) {
            bevy::platform::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            bevy::platform::collections::hash_map::Entry::Vacant(entry) => {
                let stream_id = connection.open_stream(Dir::Uni)?;

                entry.insert(NetMessageSendStreamState::new(stream_id, header))
            }
        };

        Ok(state.write(message_id, connection, message, queue)?)
    }

    /// Attempts to send a message on a connection.
    /// Returns `true` if the message was sent and `false` if it was blocked by congestion.
    /// Passing `true` into `queue` will bypass this and always queue the message the message to be sent
    /// once unblocked.
    ///
    /// Will open a new stream if this message sender doesn't have one yet.
    ///
    /// Will use the default stream header from [`NetMessageSendHeader`], if you want to choose the stream header see [`NetMessageSender::write_with_header`].
    fn write<T>(
        &mut self,
        params: &mut SenderParams,
        connection_entity: Entity,
        message_id: NetMessageId<T>,
        queue: bool,
        message: &T,
    ) -> Result<bool>
    where
        T: Serialize,
    {
        let header = params
            .stream_header
            .as_ref()
            .ok_or("Couldn't write message as `NetMessageSendHeader` resource doesn't exist")?
            .0;

        self.write_with_header(header, connection_entity, message_id, queue, message)
    }

    /// Finishes the message stream for a connection if it is not blocked on congestion.
    ///
    /// If it does not exist will do nothing.
    fn finish_if_uncongested(&mut self, connection_entity: Entity) -> Result {
        let (params, state) = self.context();

        let Some(stream_state) = state.connections.get(&connection_entity) else {
            return Ok(());
        };

        if !stream_state.uncongested() {
            return Ok(());
        }

        let (connection, connection_of) = params.connection_q.get(connection_entity)?;

        let mut endpoint = params.endpoint_q.get_mut(**connection_of)?;

        let connection = endpoint.get_connection(connection)?;

        connection.finish_send_stream(stream_state.stream_id())?;

        state.connections.remove(&connection_entity);

        Ok(())
    }

    /// Finishes any message streams that are not blocked on congestion.
    ///
    /// This is intended to be used by systems that send messages infrequently by
    fn finish_all_if_uncongested(&mut self) -> Result {
        let (params, state) = self.context();

        let mut finished_states = Vec::new();

        for (&connection_entity, stream_state) in state.connections.iter() {
            if !stream_state.uncongested() {
                continue;
            }

            let (connection, connection_of) = params.connection_q.get(connection_entity)?;

            let mut endpoint = params.endpoint_q.get_mut(**connection_of)?;

            let connection = endpoint.get_connection(connection)?;

            connection.finish_send_stream(stream_state.stream_id())?;

            finished_states.push(connection_entity);
        }

        for connection_entity in finished_states {
            state.connections.remove(&connection_entity);
        }

        Ok(())
    }

    /// Removes the state machine for a connection if it exists, giving responsibility of the stream to the caller.
    fn take_state(&mut self, connection_entity: Entity) -> Option<NetMessageSendStreamState> {
        let (_, state) = self.context();

        state.connections.remove(&connection_entity)
    }
}

impl<'w, 's> NetMessageSenderContext<'w, 's> for LocalNetMessageSender<'w, 's> {
    fn context(&mut self) -> (&mut SenderParams<'w, 's>, &mut SenderState) {
        (&mut self.params, &mut *self.state)
    }
}

impl<'w, 's> NetMessageSender<'w, 's> for LocalNetMessageSender<'w, 's> {}

impl<'w, 's, S> NetMessageSenderContext<'w, 's> for SharedNetMessageSender<'w, 's, S>
where
    S: Send + Sync + 'static,
{
    fn context(&mut self) -> (&mut SenderParams<'w, 's>, &mut SenderState) {
        (&mut self.params, &mut self.state.state)
    }
}

impl<'w, 's, S> NetMessageSender<'w, 's> for SharedNetMessageSender<'w, 's, S> where
    S: Send + Sync + 'static
{
}
