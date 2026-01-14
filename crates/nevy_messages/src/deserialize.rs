use std::{any::TypeId, collections::VecDeque};

use bevy::prelude::*;
use log::warn;
use serde::de::DeserializeOwned;

use crate::{
    MessageSystems, NevyMessagesSchedule,
    protocol::{ConnectionProtocolEntity, Protocol},
    reader::MessageStreamReaders,
};

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

pub(crate) fn build_message<T>(app: &mut App)
where
    T: Send + Sync + 'static + DeserializeOwned,
{
    app.add_observer(insert_received_messages::<T>);

    let schedule = **app.world().resource::<NevyMessagesSchedule>();

    app.add_systems(
        schedule,
        deserialize_messages::<T>.in_set(MessageSystems::DeserializeMessages),
    );
}

fn insert_received_messages<T>(
    insert: On<Insert, ConnectionProtocolEntity>,
    mut commands: Commands,
    connection_q: Query<&ConnectionProtocolEntity>,
    protocol_q: Query<&Protocol>,
) -> Result
where
    T: Send + Sync + 'static,
{
    let protocol_entity = connection_q.get(insert.entity)?;
    let protocol = protocol_q.get(**protocol_entity)?;

    if protocol.lookup.contains_key(&TypeId::of::<T>()) {
        commands
            .entity(insert.entity)
            .insert(ReceivedMessages::<T>::default());
    }

    Ok(())
}

fn deserialize_messages<T>(
    mut connection_q: Query<(
        &mut MessageStreamReaders,
        &mut ReceivedMessages<T>,
        &ConnectionProtocolEntity,
    )>,
    protocol_q: Query<&Protocol>,
) -> Result
where
    T: Send + Sync + 'static + DeserializeOwned,
{
    for (mut readers, mut deserialized_buffer, protocol_entity) in &mut connection_q {
        let protocol = protocol_q.get(**protocol_entity)?;
        let message_id = protocol
            .lookup
            .get(&TypeId::of::<T>())
            .ok_or("Protocol should have a message id for this type")?;

        let Some(serialized_buffer) = readers.buffers.get_mut(message_id) else {
            continue;
        };

        while let Some(message) = serialized_buffer.pop_front() {
            let message = match postcard::from_bytes::<T>(&message) {
                Ok(message) => message,
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
        }
    }

    Ok(())
}
