use std::{collections::VecDeque, marker::PhantomData};

use bevy::prelude::*;
use log::{debug, warn};
use nevy_transport::prelude::*;
use serde::de::DeserializeOwned;

use crate::{
    MessageSystems, NevyMessagesSchedule, protocol::MessageId, reader::MessageStreamReaders,
};

/// Can be inserted onto either a connection entity directly,
/// or onto an endpoint which which will insert it onto
/// all connections on that endpoint automatically.
#[derive(Component)]
pub struct ReceiveProtocol<P = ()> {
    _p: PhantomData<P>,
}

impl<P> Default for ReceiveProtocol<P> {
    fn default() -> Self {
        ReceiveProtocol { _p: PhantomData }
    }
}

#[derive(Component)]
#[require(MessageStreamReaders)]
pub struct ReceivedMessages<T> {
    messages: VecDeque<T>,
}

impl<T> Default for ReceivedMessages<T> {
    fn default() -> Self {
        ReceivedMessages {
            messages: VecDeque::new(),
        }
    }
}

impl<T> ReceivedMessages<T> {
    pub fn drain(&mut self) -> impl Iterator<Item = T> {
        self.messages.drain(..)
    }
}

pub(crate) fn build<P>(app: &mut App)
where
    P: Send + Sync + 'static,
{
    app.add_observer(insert_receive_protocols::<P>);
}

pub(crate) fn build_message<T, P>(app: &mut App)
where
    T: Send + Sync + 'static + DeserializeOwned,
    P: Send + Sync + 'static,
{
    app.add_observer(insert_received_messages::<T, P>);

    let schedule = **app.world().resource::<NevyMessagesSchedule>();

    app.add_systems(
        schedule,
        deserialize_messages::<T, P>.in_set(MessageSystems::DeserializeMessages),
    );
}

/// When a [`ConnectionOf`] is inserted and it's endpoint has a [`ReceiveProtocol<P>`],
/// insert a [`ReceiveProtocol<P>`] onto the connection.
fn insert_receive_protocols<P>(
    insert: On<Insert, ConnectionOf>,
    mut commands: Commands,
    connection_q: Query<&ConnectionOf>,
    endpoint_q: Query<(), With<ReceiveProtocol<P>>>,
) -> Result
where
    P: Send + Sync + 'static,
{
    let &ConnectionOf(endpoint_entity) = connection_q.get(insert.entity)?;

    if endpoint_q.contains(endpoint_entity) {
        commands
            .entity(insert.entity)
            .insert(ReceiveProtocol::<P>::default());
    }

    Ok(())
}

/// When a [`ReceiveProtocol<P>`] is inserted onto an entity with a [`ConnectionOf`]
/// insert the [`ReceivedMessages<T, P>`] buffer this observer is responsible for.
fn insert_received_messages<T, P>(
    insert: On<Insert, ReceiveProtocol<P>>,
    mut commands: Commands,
    connection_q: Query<(), With<ConnectionOf>>,
) -> Result
where
    T: Send + Sync + 'static,
    P: Send + Sync + 'static,
{
    if connection_q.contains(insert.entity) {
        commands
            .entity(insert.entity)
            .insert(ReceivedMessages::<T>::default());
    }

    Ok(())
}

fn deserialize_messages<T, P>(
    mut connection_q: Query<
        (&mut MessageStreamReaders, &mut ReceivedMessages<T>),
        With<ReceiveProtocol<P>>,
    >,
    message_id: Res<MessageId<T, P>>,
) where
    T: Send + Sync + 'static + DeserializeOwned,
    P: Send + Sync + 'static,
{
    for (mut readers, mut deserialized_buffer) in &mut connection_q {
        let Some(serialized_buffer) = readers.buffers.get_mut(&message_id.id) else {
            continue;
        };

        while let Some(message) = serialized_buffer.pop_front() {
            let message = match bincode::serde::decode_from_slice(&message, crate::bincode_config())
            {
                Ok((message, _)) => message,
                Err(err) => {
                    warn!(
                        "Failed to deserialize message of type {}: {}",
                        std::any::type_name::<T>(),
                        err
                    );
                    continue;
                }
            };

            deserialized_buffer.messages.push_back(message);

            debug!("Deserialized a message");
        }
    }
}
