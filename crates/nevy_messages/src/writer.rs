use std::{any::TypeId, collections::VecDeque, marker::PhantomData};

use bevy::{
    ecs::system::SystemParam,
    platform::collections::{HashMap, hash_map::Entry},
    prelude::*,
};
use bytes::Bytes;
use nevy_transport::{
    prelude::*,
    varint::{EncodedVarInt, VarInt},
};
use serde::Serialize;

use crate::protocol::{ConnectionProtocolEntity, Protocol};

/// A state machine for a stream that sends messages.
///
/// For most use cases that involve sending messages use either [`LocalMessageSender`] or [`SharedMessageSender`].
pub struct MessageStreamState {
    stream: Stream,
    /// Each chunk of data must be sent entirely.
    /// This is why they are kept separately.
    buffer: VecDeque<Bytes>,
    /// The stream header, written before the next message.
    header: Option<EncodedVarInt>,
    /// Whether [`Self::header`] is written before every message instead of only the first.
    header_per_chunk: bool,
}

impl MessageStreamState {
    pub fn new(mut connection: Connection, requirements: StreamRequirements) -> Result<Self> {
        let stream = connection.new_stream(requirements)?;

        Ok(MessageStreamState {
            stream,
            buffer: VecDeque::new(),
            header: None,
            header_per_chunk: false,
        })
    }

    /// Opens a stream that starts with a stream header.
    #[cfg(feature = "stream_headers")]
    pub fn new_with_header(
        mut connection: Connection,
        requirements: StreamRequirements,
        header: impl Into<StreamHeader>,
    ) -> Result<Self> {
        let (stream, header) = connection.new_headered_stream(requirements, header.into())?;

        Ok(MessageStreamState {
            stream,
            buffer: VecDeque::new(),
            header: Some(header),
            header_per_chunk: StreamHeader::per_chunk(requirements),
        })
    }

    /// Writes as much data as possible, returns `true` if all data was written.
    pub fn flush(&mut self, mut connection: Connection) -> Result<bool> {
        while let Some(message) = self.buffer.front_mut() {
            let written = connection.write(&self.stream, message.clone(), true)?;
            let _ = message.split_to(written);

            if message.is_empty() {
                self.buffer.pop_front();
            } else {
                return Ok(false);
            }
        }

        Ok(true)
    }

    pub fn write<T>(
        &mut self,
        mut connection: Connection,
        message_id: usize,
        queue: bool,
        message: &T,
    ) -> Result<bool>
    where
        T: Serialize,
    {
        let buffer_empty = self.flush(connection.reborrow())?;

        // Only attempt to send the message if there is no congestion or if `queue` is true.
        if !(queue || buffer_empty) {
            return Ok(false);
        }

        let message = postcard::to_stdvec(message)?;

        let header = match self.header_per_chunk {
            true => self.header,
            false => self.header.take(),
        };

        // + 24 is space for header, message id and message length.
        // Exact size not important for such a small number,
        // just avoiding double allocation when more space is needed.
        let mut buffer = Vec::with_capacity(message.len() + 24);

        if let Some(header) = header {
            buffer.extend_from_slice(&header);
        }

        VarInt::from_u64(message_id as u64)
            .ok_or("Message id was too big for VarInt")?
            .encode_into(&mut buffer);

        VarInt::from_u64(message.len() as u64)
            .ok_or("Message length was too big for VarInt")?
            .encode_into(&mut buffer);

        buffer.extend(message);

        self.buffer.push_back(buffer.into());

        self.flush(connection)?;

        Ok(true)
    }

    pub fn close(&mut self, mut connection: Connection, graceful: bool) -> Result {
        connection.close_send_stream(&self.stream, graceful)
    }
}

/// Holds an optional [`MessageStreamState`] for each connection.
/// Used by either a [`LocalMessageSender`] or [`SharedMessageSender`] for sending messages from systems.
#[derive(Default)]
pub struct MessageSenderState {
    streams: HashMap<Entity, MessageStreamState>,
}

/// System parameters needed for either a [`LocalMessageSender`] or [`SharedMessageSender`] to send messages.
#[derive(SystemParam)]
pub struct MessageSenderParams<'w, 's> {
    connection_q: Query<'w, 's, (&'static ConnectionOf, &'static ConnectionProtocolEntity)>,
    endpoint_q: Query<'w, 's, &'static mut Endpoint>,
    protocol_q: Query<'w, 's, &'static Protocol>,
    #[cfg(feature = "stream_headers")]
    header: Option<Res<'w, MessageStreamHeader>>,
}

/// The stream header every message stream is opened with,
/// inserted when messages use [`MessageStreams::Header`](crate::MessageStreams::Header).
#[cfg(feature = "stream_headers")]
#[derive(Resource, Deref, Clone, Copy)]
pub(crate) struct MessageStreamHeader(pub StreamHeader);

impl MessageSenderParams<'_, '_> {
    #[cfg(feature = "stream_headers")]
    fn header(&self) -> Option<StreamHeader> {
        return self.header.as_ref().map(|header| ***header);
    }
}

/// Opens a message stream, with the configured stream header if one is provided.
fn new_message_stream(
    #[cfg(feature = "stream_headers")] header: Option<StreamHeader>,
    connection: Connection,
    requirements: StreamRequirements,
) -> Result<MessageStreamState> {
    #[cfg(feature = "stream_headers")]
    if let Some(header) = header {
        return MessageStreamState::new_with_header(connection, requirements, header);
    }

    MessageStreamState::new(connection, requirements)
}

/// Holds a [`MessageSenderState`] in a system [`Local`].
/// Used for sending messages when ordering requirements between systems aren't needed.
///
/// [`Self::flush`](MessageSender::flush) needs to be called in any system that uses this sender,
/// or partially written messages will not be sent.
///
/// By default this sender is reliable and ordered. There are shorthands for less strict [`StreamRequirements`].
/// - [`LocalMessageSenderUnrel`]
/// - [`LocalMessageSenderUnord`]
/// - [`LocalMessageSenderUnordUnrel`]
#[derive(SystemParam)]
pub struct LocalMessageSender<'w, 's, const RELIABLE: bool = true, const ORDERED: bool = true> {
    state: Local<'s, MessageSenderState>,
    params: MessageSenderParams<'w, 's>,
}

pub type LocalMessageSenderUnrel<'w, 's> = LocalMessageSender<'w, 's, true, false>;
pub type LocalMessageSenderUnord<'w, 's> = LocalMessageSender<'w, 's, false, true>;
pub type LocalMessageSenderUnordUnrel<'w, 's> = LocalMessageSender<'w, 's, false, false>;

/// Holds a [`MessageSenderState`] in a shared resource.
/// Used for sending messages when ordering requirements between systems are needed.
///
/// Unlike a [`LocalMessageSender`] the shared state needs to be initialized by calling
/// [`App::add_shared_message_sender<S>`](AddSharedMessageSender::add_shared_message_sender)
/// with the desired [`StreamRequirements`].
///
/// Each shared sender is marked by a unique `S`.
///
/// [`Self::flush`](MessageSender::flush) does not need to be called manually for shared senders, it is done automatically once per tick.
#[derive(SystemParam)]
pub struct SharedMessageSender<'w, 's, S>
where
    S: Send + Sync + 'static,
{
    state: ResMut<'w, SharedMessageSenderState<S>>,
    params: MessageSenderParams<'w, 's>,
}

/// The shared state used by a [`SharedMessageSender`].
#[derive(Resource)]
struct SharedMessageSenderState<S> {
    _p: PhantomData<S>,
    requirements: StreamRequirements,
    state: MessageSenderState,
}

pub trait MessageSender<'w, 's> {
    fn context(
        &mut self,
    ) -> (
        &mut MessageSenderState,
        &mut MessageSenderParams<'w, 's>,
        StreamRequirements,
    );

    /// Attempts to flush any partially written messages.
    fn flush(&mut self) -> Result {
        let (state, params, _) = self.context();

        let mut remove_streams = Vec::new();

        for (&connection_entity, stream_state) in &mut state.streams {
            let Ok((&ConnectionOf(endpoint_entity), _)) =
                params.connection_q.get(connection_entity)
            else {
                // If the connection no longer exists remove the stream.
                remove_streams.push(connection_entity);
                continue;
            };

            let mut endpoint = params.endpoint_q.get_mut(endpoint_entity)?;
            let connection = endpoint.get_connection(connection_entity)?;

            stream_state.flush(connection)?;
        }

        for connection_entity in remove_streams {
            state.streams.remove(&connection_entity);
        }

        Ok(())
    }

    /// Attempts to write a message on the current stream.
    ///
    /// If `queue` is true the message will be queued to send even if the stream is congested.
    /// Shared senders share a queue.
    fn write<T>(&mut self, connection_entity: Entity, queue: bool, message: &T) -> Result<bool>
    where
        T: Serialize + 'static,
    {
        let (state, params, requirements) = self.context();

        let (&ConnectionOf(endpoint_entity), protocol_entity) = params
            .connection_q
            .get(connection_entity)
            .ok()
            .ok_or("Entity is not a connection with a messaging protocol.")?;

        let protocol = params.protocol_q.get(**protocol_entity)?;
        let &message_id = protocol.lookup.get(&TypeId::of::<T>()).ok_or_else(|| {
            format!(
                "This connection's protocol doesn't have an id assigned for message `{}`",
                std::any::type_name::<T>()
            )
        })?;

        #[cfg(feature = "stream_headers")]
        let header = params.header();
        let mut endpoint = params.endpoint_q.get_mut(endpoint_entity)?;
        let mut connection = endpoint.get_connection(connection_entity)?;

        let stream_state = match state.streams.entry(connection_entity) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                #[cfg(feature = "stream_headers")]
                let stream_state = new_message_stream(header, connection.reborrow(), requirements)?;
                #[cfg(not(feature = "stream_headers"))]
                let stream_state = new_message_stream(connection.reborrow(), requirements)?;

                entry.insert(stream_state)
            }
        };

        stream_state.write(connection, message_id, queue, message)
    }

    /// Returns the number of buffered messages for a connection.
    fn queue_size(&mut self, connection_entity: Entity) -> usize {
        let (state, _, _) = self.context();
        state
            .streams
            .get(&connection_entity)
            .map(|state| state.buffer.len())
            .unwrap_or(0)
    }

    /// Closes all streams that don't have queued data gracefully.
    fn close_unused_streams(&mut self) -> Result {
        let (state, params, _) = self.context();

        let mut remove_streams = Vec::new();

        for (&connection_entity, state) in &mut state.streams {
            if !state.buffer.is_empty() {
                continue;
            }

            let Ok((&ConnectionOf(endpoint_entity), _)) =
                params.connection_q.get(connection_entity)
            else {
                remove_streams.push(connection_entity);
                continue;
            };

            let mut endpoint = params.endpoint_q.get_mut(endpoint_entity)?;
            let connection = endpoint.get_connection(connection_entity)?;

            state.close(connection, true)?;

            remove_streams.push(connection_entity);
        }

        for connection_entity in remove_streams {
            state.streams.remove(&connection_entity);
        }

        Ok(())
    }

    /// Closes all streams, dropping any queued messages that haven't been sent.
    fn close_all_streams(&mut self, graceful: bool) -> Result {
        let (state, params, _) = self.context();

        for (connection_entity, mut state) in std::mem::take(&mut state.streams) {
            let Ok((&ConnectionOf(endpoint_entity), _)) =
                params.connection_q.get(connection_entity)
            else {
                continue;
            };

            let mut endpoint = params.endpoint_q.get_mut(endpoint_entity)?;
            let connection = endpoint.get_connection(connection_entity)?;

            state.close(connection, graceful)?;
        }

        Ok(())
    }
}

impl<'w, 's, const RELIABLE: bool, const ORDERED: bool> MessageSender<'w, 's>
    for LocalMessageSender<'w, 's, RELIABLE, ORDERED>
{
    fn context(
        &mut self,
    ) -> (
        &mut MessageSenderState,
        &mut MessageSenderParams<'w, 's>,
        StreamRequirements,
    ) {
        let requirements = StreamRequirements {
            reliable: RELIABLE,
            ordered: ORDERED,
            bidirectional: false,
        };

        (&mut *self.state, &mut self.params, requirements)
    }
}

impl<'w, 's, S> MessageSender<'w, 's> for SharedMessageSender<'w, 's, S>
where
    S: Send + Sync + 'static,
{
    fn context(
        &mut self,
    ) -> (
        &mut MessageSenderState,
        &mut MessageSenderParams<'w, 's>,
        StreamRequirements,
    ) {
        let requirements = self.state.requirements;

        (&mut self.state.state, &mut self.params, requirements)
    }
}

pub trait AddSharedMessageSender {
    fn add_shared_message_sender<S>(&mut self, requirements: StreamRequirements)
    where
        S: Send + Sync + 'static;
}

impl AddSharedMessageSender for App {
    /// Inserts the shared state for a [`SharedMessageSender<S>`].
    fn add_shared_message_sender<S>(&mut self, requirements: StreamRequirements)
    where
        S: Send + Sync + 'static,
    {
        self.insert_resource(SharedMessageSenderState::<S> {
            _p: PhantomData,
            requirements: requirements.with_ordered(true),
            state: MessageSenderState::default(),
        });

        self.add_systems(
            DEFAULT_TRANSPORT_SCHEDULE,
            flush_shared_message_sender::<S>.before(TransportUpdateSystems),
        );
    }
}

fn flush_shared_message_sender<S>(mut sender: SharedMessageSender<'_, '_, S>) -> Result
where
    S: Send + Sync + 'static,
{
    sender.flush()
}
