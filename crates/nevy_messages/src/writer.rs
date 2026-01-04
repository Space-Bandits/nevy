use bevy::{
    ecs::system::SystemParam,
    platform::collections::{HashMap, hash_map::Entry},
    prelude::*,
};
use bytes::Bytes;
use nevy_transport::prelude::*;
use serde::Serialize;

use crate::{protocol::MessageId, varint::VarInt};

pub struct MessageStreamState {
    stream: Stream,
    buffer: Option<Bytes>,
}

impl MessageStreamState {
    pub fn new(mut connection: Connection) -> Result<Self> {
        let stream = connection.new_stream(StreamRequirements::RELIABLE_ORDERED)?;

        Ok(MessageStreamState {
            stream,
            buffer: None,
        })
    }

    pub fn flush(&mut self, mut connection: Connection) -> Result {
        let Some(bytes) = self.buffer.as_mut() else {
            return Ok(());
        };

        let written = connection.write(&self.stream, bytes.clone(), true)?;
        let _ = bytes.split_to(written);

        if bytes.is_empty() {
            self.buffer = None;
        }

        Ok(())
    }

    pub fn write<T, P>(
        &mut self,
        mut connection: Connection,
        message_id: MessageId<T, P>,
        message: &T,
    ) -> Result<bool>
    where
        T: Serialize,
    {
        self.flush(connection.reborrow())?;

        if self.buffer.is_some() {
            return Ok(false);
        }

        let message = bincode::serde::encode_to_vec(message, crate::bincode_config())?;

        let mut buffer = Vec::new();

        VarInt::from_u64(message_id.id as u64)
            .ok_or("Message id was too big for VarInt")?
            .encode(&mut buffer);

        VarInt::from_u64(message.len() as u64)
            .ok_or("Message length was too big for VarInt")?
            .encode(&mut buffer);

        buffer.extend(message);

        self.buffer = Some(buffer.into());

        self.flush(connection)?;

        Ok(true)
    }
}

#[derive(Default)]
pub struct MessageSenderState {
    streams: HashMap<Entity, MessageStreamState>,
}

#[derive(SystemParam)]
pub struct MessageSenderParams<'w, 's> {
    connection_q: Query<'w, 's, &'static ConnectionOf>,
    endpoint_q: Query<'w, 's, &'static mut Endpoint>,
}

#[derive(SystemParam)]
pub struct LocalMessageSender<'w, 's> {
    state: Local<'s, MessageSenderState>,
    params: MessageSenderParams<'w, 's>,
}

#[derive(Resource)]
struct SharedMessageSenderState(MessageSenderState);

#[derive(SystemParam)]
pub struct SharedMessageSender<'w, 's> {
    state: ResMut<'w, SharedMessageSenderState>,
    params: MessageSenderParams<'w, 's>,
}

pub trait MessageSender<'a> {
    fn context(
        &'a mut self,
    ) -> (
        &'a mut MessageSenderState,
        &'a mut MessageSenderParams<'a, 'a>,
    );

    fn flush(&'a mut self) -> Result {
        let (state, params) = self.context();

        let mut remove_streams = Vec::new();

        for (&connection_entity, stream_state) in &mut state.streams {
            let Ok(&ConnectionOf(endpoint_entity)) = params.connection_q.get(connection_entity)
            else {
                // If the connection no longer exists remove the stream.
                remove_streams.push(connection_entity);
                continue;
            };

            let mut endpoint = params.endpoint_q.get_mut(endpoint_entity)?;
            let connection = endpoint.get_connection(connection_entity)?;

            stream_state.flush(connection)?
        }

        for connection_entity in remove_streams {
            state.streams.remove(&connection_entity);
        }

        Ok(())
    }

    fn write<T, P>(
        &'a mut self,
        connection_entity: Entity,
        message_id: MessageId<T, P>,
        message: &T,
    ) -> Result<bool>
    where
        T: Serialize,
    {
        let (state, params) = self.context();

        let &ConnectionOf(endpoint_entity) = params.connection_q.get(connection_entity)?;
        let mut endpoint = params.endpoint_q.get_mut(endpoint_entity)?;
        let mut connection = endpoint.get_connection(connection_entity)?;

        let stream_state = match state.streams.entry(connection_entity) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(MessageStreamState::new(connection.reborrow())?),
        };

        stream_state.write(connection, message_id, message)
    }
}

impl<'a, 'w, 's> MessageSender<'a> for LocalMessageSender<'w, 's>
where
    'a: 'w + 's,
{
    fn context(
        &'a mut self,
    ) -> (
        &'a mut MessageSenderState,
        &'a mut MessageSenderParams<'a, 'a>,
    ) {
        (&mut *self.state, &mut self.params)
    }
}

impl<'a, 'w, 's> MessageSender<'a> for SharedMessageSender<'w, 's>
where
    'a: 'w + 's,
{
    fn context(
        &'a mut self,
    ) -> (
        &'a mut MessageSenderState,
        &'a mut MessageSenderParams<'a, 'a>,
    ) {
        (&mut self.state.0, &mut self.params)
    }
}
