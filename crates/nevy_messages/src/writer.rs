use std::{any::TypeId, marker::PhantomData};

use bevy::{
    ecs::system::SystemParam,
    platform::collections::{HashMap, hash_map::Entry},
    prelude::*,
};
use bytes::Bytes;
use nevy_transport::prelude::*;
use serde::Serialize;

use crate::{
    protocol::{ConnectionProtocolEntity, Protocol},
    varint::VarInt,
};

pub struct MessageStreamState {
    stream: Stream,
    buffer: Option<Bytes>,
}

impl MessageStreamState {
    pub fn new(mut connection: Connection, requirements: StreamRequirements) -> Result<Self> {
        let stream = connection.new_stream(requirements)?;

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

    pub fn write<T>(
        &mut self,
        mut connection: Connection,
        message_id: usize,
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

        VarInt::from_u64(message_id as u64)
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
    connection_q: Query<'w, 's, (&'static ConnectionOf, &'static ConnectionProtocolEntity)>,
    endpoint_q: Query<'w, 's, &'static mut Endpoint>,
    protocol_q: Query<'w, 's, &'static Protocol>,
}

/*
*
* struct Streams(Vec<(MessageStreamState)>)
*
* fn my_func(sender: MessageSender) {
*  sender.send(stream, message);
}
*/

#[derive(SystemParam)]
pub struct LocalMessageSender<'w, 's, const RELIABLE: bool = true, const ORDERED: bool = true> {
    state: Local<'s, MessageSenderState>,
    params: MessageSenderParams<'w, 's>,
}

pub type LocalMessageSenderUnrel<'w, 's> = LocalMessageSender<'w, 's, true, false>;
pub type LocalMessageSenderUnord<'w, 's> = LocalMessageSender<'w, 's, false, true>;
pub type LocalMessageSenderUnordUnrel<'w, 's> = LocalMessageSender<'w, 's, false, false>;

#[derive(Resource)]
struct SharedMessageSenderState<S> {
    _p: PhantomData<S>,
    requirements: StreamRequirements,
    state: MessageSenderState,
}

#[derive(SystemParam)]
pub struct SharedMessageSender<'w, 's, S>
where
    S: Send + Sync + 'static,
{
    state: ResMut<'w, SharedMessageSenderState<S>>,
    params: MessageSenderParams<'w, 's>,
}

pub trait MessageSender<'w, 's> {
    fn context(
        &mut self,
    ) -> (
        &mut MessageSenderState,
        &mut MessageSenderParams<'w, 's>,
        StreamRequirements,
    );

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

            stream_state.flush(connection)?
        }

        for connection_entity in remove_streams {
            state.streams.remove(&connection_entity);
        }

        Ok(())
    }

    fn write<T>(&mut self, connection_entity: Entity, message: &T) -> Result<bool>
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
        let &message_id = protocol
            .lookup
            .get(&TypeId::of::<T>())
            .ok_or("This connection's protocol doesn't have an id assigned for this message")?;

        let mut endpoint = params.endpoint_q.get_mut(endpoint_entity)?;
        let mut connection = endpoint.get_connection(connection_entity)?;

        let stream_state = match state.streams.entry(connection_entity) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(MessageStreamState::new(
                connection.reborrow(),
                requirements,
            )?),
        };

        stream_state.write(connection, message_id, message)
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
    }
}
