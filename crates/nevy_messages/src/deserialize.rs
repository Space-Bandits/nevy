use std::{collections::VecDeque, marker::PhantomData};

use bevy::prelude::*;
use nevy_transport::prelude::*;

use crate::protocol::MessageIdCount;

#[derive(Component)]
pub struct ReceivedSerializedMessages<P> {
    _p: PhantomData<P>,
    messages: Box<[VecDeque<Box<[u8]>>]>,
}

#[derive(Component)]
pub struct ReceivedMessages<T, P = ()> {
    _p: PhantomData<P>,
    messages: VecDeque<T>,
}

impl<T, P> Default for ReceivedMessages<T, P> {
    fn default() -> Self {
        ReceivedMessages {
            _p: PhantomData,
            messages: VecDeque::new(),
        }
    }
}

#[derive(Component)]
pub struct AddReceivedMessages<P = ()> {
    _p: PhantomData<P>,
}

impl<P> Default for AddReceivedMessages<P> {
    fn default() -> Self {
        AddReceivedMessages { _p: PhantomData }
    }
}

pub(crate) fn build<P>(app: &mut App)
where
    P: Send + Sync + 'static,
{
    app.add_observer(add_serialized_message_buffer::<P>);
}

pub(crate) fn build_message<T, P>(app: &mut App)
where
    T: Send + Sync + 'static,
    P: Send + Sync + 'static,
{
    app.add_observer(add_message_buffer::<T, P>);
}

fn add_serialized_message_buffer<P>(
    insert: On<Insert, ConnectionOf>,
    mut commands: Commands,
    connection_q: Query<&ConnectionOf>,
    endpoint_q: Query<(), With<AddReceivedMessages<P>>>,
    count: Res<MessageIdCount<P>>,
) -> Result
where
    P: Send + Sync + 'static,
{
    let &ConnectionOf(endpoint_entity) = connection_q.get(insert.entity)?;

    if endpoint_q.contains(endpoint_entity) {
        commands
            .entity(insert.entity)
            .insert(ReceivedSerializedMessages::<P> {
                _p: PhantomData,
                messages: vec![VecDeque::new(); count.count].into_boxed_slice(),
            });
    }

    Ok(())
}

fn add_message_buffer<T, P>(
    insert: On<Insert, ReceivedSerializedMessages<P>>,
    mut commands: Commands,
) where
    T: Send + Sync + 'static,
    P: Send + Sync + 'static,
{
    commands
        .entity(insert.entity)
        .insert(ReceivedMessages::<T, P>::default());
}
