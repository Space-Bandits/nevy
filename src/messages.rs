fn bincode_config() -> bincode::config::Configuration {
    bincode::config::standard()
}

#[derive(Resource, Default)]
struct NextMessageId(u16);

pub trait AddMessage {
    fn add_message<T>(&mut self)
    where
        T: Serialize + DeserializeOwned + Send + Sync + 'static;
}

impl AddMessage for App {
    fn add_message<T>(&mut self)
    where
        T: Serialize + DeserializeOwned,
        MessageId<T>: Resource,
        ReceivedMessages<T>: Component<Mutability = Mutable>,
    {
        let mut next_message_id = self.world_mut().get_resource_or_init::<NextMessageId>();

        let message_id = next_message_id.0;
        next_message_id.0 += 1;

        self.insert_resource(MessageId::<T> {
            _p: PhantomData,
            id: message_id,
        });

        self.add_systems(
            PostUpdate,
            (
                insert_received_message_buffers::<T>.in_set(NetworkingSet::InsertComponents),
                deserialize_messages::<T>.in_set(NetworkingSet::DeserializeMessages),
            ),
        );
    }
}

/// Holds the id of a message, needs to be provided when sending a message.
#[derive(Resource)]
pub struct MessageId<T> {
    _p: PhantomData<T>,
    id: u16,
}

impl<T> Clone for MessageId<T> {
    fn clone(&self) -> Self {
        Self {
            _p: PhantomData,
            id: self.id,
        }
    }
}

impl<T> Copy for MessageId<T> {}

/// Holds the state machines for the message streams of a connection.
#[derive(Component, Default)]
pub(crate) struct MessageRecvStreamStates {
    streams: HashMap<StreamId, MessageRecvStreamState>,
    messages: HashMap<u16, VecDeque<Box<[u8]>>>,
}

/// State machine for a single message stream on a connection.
enum MessageRecvStreamState {
    ReadingId {
        buffer: U16Reader,
    },
    ReadingLength {
        message_id: u16,
        buffer: U16Reader,
    },
    ReadingMessage {
        message_id: u16,
        length: u16,
        buffer: Vec<u8>,
    },
}

/// Buffer of received messages from all streams for a connection.
#[derive(Component)]
pub struct ReceivedMessages<T> {
    messages: VecDeque<T>,
}

pub(crate) fn insert_recv_stream_buffers(
    mut commands: Commands,
    connection_q: Query<Entity, Added<ConnectionOf>>,
) {
    for connection_entity in &connection_q {
        commands
            .entity(connection_entity)
            .insert(MessageRecvStreamStates::default());
    }
}

pub(crate) fn insert_received_message_buffers<T>(
    mut commands: Commands,
    connection_q: Query<Entity, Added<ConnectionOf>>,
) where
    ReceivedMessages<T>: Component,
{
    for connection_entity in &connection_q {
        commands
            .entity(connection_entity)
            .insert(ReceivedMessages::<T> {
                messages: VecDeque::new(),
            });
    }
}

pub(crate) fn take_message_streams(
    mut connection_q: Query<(Entity, &mut RecvStreamHeaders, &mut MessageRecvStreamStates)>,
) {
    for (connection_entity, mut headers, mut recv_streams) in &mut connection_q {
        while let Some((stream_id, dir)) = headers.take_stream(StreamHeader::Messages) {
            if let Dir::Bi = dir {
                warn!(
                    "Connection {} opened a bidirectional message stream which should only ever be unidirectional.",
                    connection_entity
                );
            }

            recv_streams.streams.insert(
                stream_id,
                MessageRecvStreamState::ReadingId {
                    buffer: U16Reader::new(),
                },
            );
        }
    }
}

pub(crate) fn read_message_streams(
    mut connection_q: Query<(
        Entity,
        &ConnectionOf,
        &QuicConnection,
        &mut MessageRecvStreamStates,
    )>,
    mut endpoint_q: Query<&mut QuicEndpoint>,
) -> Result {
    for (connection_entity, connection_of, connection, mut recv_streams) in &mut connection_q {
        let buffers = recv_streams.as_mut();

        let mut endpoint = endpoint_q.get_mut(**connection_of)?;

        let connection = endpoint.get_connection(connection)?;

        let mut finished = Vec::new();

        for (&stream_id, stream_state) in buffers.streams.iter_mut() {
            loop {
                let bytes_needed = match stream_state {
                    MessageRecvStreamState::ReadingId { buffer } => buffer.bytes_needed(),
                    MessageRecvStreamState::ReadingLength { buffer, .. } => buffer.bytes_needed(),
                    MessageRecvStreamState::ReadingMessage { buffer, length, .. } => {
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
                    MessageRecvStreamState::ReadingId { buffer } => {
                        buffer.write(&chunk.data);

                        let Some(message_id) = buffer.finish() else {
                            continue;
                        };

                        *stream_state = MessageRecvStreamState::ReadingLength {
                            message_id,
                            buffer: U16Reader::new(),
                        };
                    }
                    MessageRecvStreamState::ReadingLength { message_id, buffer } => {
                        buffer.write(&chunk.data);

                        let Some(length) = buffer.finish() else {
                            continue;
                        };

                        *stream_state = MessageRecvStreamState::ReadingMessage {
                            message_id: *message_id,
                            length,
                            buffer: Vec::new(),
                        };
                    }
                    MessageRecvStreamState::ReadingMessage {
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

                        *stream_state = MessageRecvStreamState::ReadingId {
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

pub(crate) fn deserialize_messages<T>(
    message_id: Res<MessageId<T>>,
    mut connection_q: Query<(&mut MessageRecvStreamStates, &mut ReceivedMessages<T>)>,
) where
    T: DeserializeOwned,
    MessageId<T>: Resource,
    ReceivedMessages<T>: Component<Mutability = Mutable>,
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
                    error!(
                        "Failed to deserialize \"{}\" message: {}",
                        std::any::type_name::<T>(),
                        error
                    );
                }
            }
        }
    }
}

impl<T> ReceivedMessages<T> {
    pub fn drain(&mut self) -> impl Iterator<Item = T> {
        self.messages.drain(..)
    }

    pub fn next(&mut self) -> Option<T> {
        self.messages.pop_front()
    }
}

/// State machine for a stream that sends messages.
pub struct MessageSendStreamState {
    stream: HeaderedStreamState,
    buffer: VecDeque<u8>,
}

impl MessageSendStreamState {
    /// Creates a new state machine that will send data on a stream.
    ///
    /// Message streams should be unidirectional.
    pub fn new(stream_id: StreamId) -> Self {
        Self {
            stream: HeaderedStreamState::new(stream_id, StreamHeader::Messages),
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
        message_id: MessageId<T>,
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
        let message_length: u16 = message_data.len().try_into().expect("Message was too long");
        self.buffer.extend(message_length.to_be_bytes());

        // write the message
        self.buffer.extend(message_data);

        self.flush(connection)?;

        Ok(true)
    }
}

/// Ecs system parameter that holds send message state machines for connections.
#[derive(SystemParam)]
pub struct MessageSender<'w, 's> {
    connection_q: Query<'w, 's, (&'static QuicConnection, &'static ConnectionOf)>,
    endpoint_q: Query<'w, 's, &'static mut QuicEndpoint>,
    states: Local<'s, EntityHashMap<MessageSendStreamState>>,
}

impl<'w, 's> MessageSender<'w, 's> {
    /// Drives state machines for all connections to completion.
    ///
    /// Should be called regularly to ensure messages that are still in buffers are sent.
    pub fn flush(&mut self) -> Result {
        let mut removed_connections = Vec::new();

        for (&connection_entity, state) in self.states.iter_mut() {
            let Ok((connection, connection_of)) = self.connection_q.get(connection_entity) else {
                removed_connections.push(connection_entity);

                continue;
            };

            let mut endpoint = self.endpoint_q.get_mut(**connection_of)?;

            let connection = endpoint.get_connection(connection)?;

            state.flush(connection)?;
        }

        for connection_entity in removed_connections {
            self.states.remove(&connection_entity);
        }

        Ok(())
    }

    /// Sends a message on a connection.
    ///
    /// Will open a new stream if this message sender doesn't have one yet.
    pub fn write<T>(
        &mut self,
        connection_entity: Entity,
        message_id: MessageId<T>,
        message: &T,
        queue: bool,
    ) -> Result
    where
        T: Serialize,
    {
        let (connection, connection_of) = self.connection_q.get(connection_entity)?;

        let mut endpoint = self.endpoint_q.get_mut(**connection_of)?;

        let connection = endpoint.get_connection(connection)?;

        let state = match self.states.entry(connection_entity) {
            bevy::platform::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            bevy::platform::collections::hash_map::Entry::Vacant(entry) => {
                let stream_id = connection.open_stream(Dir::Uni)?;

                entry.insert(MessageSendStreamState::new(stream_id))
            }
        };

        state.write(message_id, connection, message, queue)?;

        Ok(())
    }

    /// Finishes the message stream for a connection if it exists and all messages have been sent.
    pub fn finish_if_uncongested(&mut self, connection_entity: Entity) -> Result {
        let Some(state) = self.states.get(&connection_entity) else {
            return Ok(());
        };

        if !state.uncongested() {
            return Ok(());
        }

        let (connection, connection_of) = self.connection_q.get(connection_entity)?;

        let mut endpoint = self.endpoint_q.get_mut(**connection_of)?;

        let connection = endpoint.get_connection(connection)?;

        connection.finish_send_stream(state.stream_id())?;

        self.states.remove(&connection_entity);

        Ok(())
    }

    /// Removes the state machine for a connection if it exists,
    /// giving responsibility of the stream to the caller.
    pub fn take_state(&mut self, connection_entity: Entity) -> Option<MessageSendStreamState> {
        self.states.remove(&connection_entity)
    }
}
