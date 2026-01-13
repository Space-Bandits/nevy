use std::{any::TypeId, collections::HashMap, marker::PhantomData};

use bevy::prelude::*;
use nevy_transport::prelude::*;
use serde::de::DeserializeOwned;

use crate::deserialize;

/// Resource that points to the [`Protocol`] [`Entity`] that holds the message ids for a protocol `P`
#[derive(Resource, Deref)]
pub(crate) struct ProtocolEntity<P> {
    _p: PhantomData<P>,
    #[deref]
    pub protocol_entity: Entity,
}

/// Holds type erased [`TypeId`]s for a protocol.
#[derive(Component, Default)]
pub(crate) struct Protocol {
    pub messages: Vec<TypeId>,
    pub lookup: HashMap<TypeId, usize>,
}

/// Can be inserted onto either a connection entity directly,
/// or onto an endpoint which which will insert it onto
/// all connections on that endpoint automatically.
#[derive(Component)]
pub struct ConnectionProtocol<P> {
    _p: PhantomData<P>,
}

impl<P> Default for ConnectionProtocol<P> {
    fn default() -> Self {
        ConnectionProtocol { _p: PhantomData }
    }
}

/// When a [`ConnectionProtocol<P>`] is inserted onto a [`ConnectionOf`]
/// this component is inserted which points to the [`Protocol`] [`Entity`] for protocol `P`.
#[derive(Component, Deref)]
pub(crate) struct ConnectionProtocolEntity(Entity);

pub trait ProtocolBuilder {
    fn init_protocol<P>(&mut self)
    where
        P: Send + Sync + 'static;

    fn add_protocol_message<P, T>(&mut self)
    where
        P: Send + Sync + 'static,
        T: Send + Sync + 'static + DeserializeOwned;

    fn include_protocol<P, O>(&mut self)
    where
        P: Send + Sync + 'static,
        O: Send + Sync + 'static;
}

impl ProtocolBuilder for App {
    fn init_protocol<P>(&mut self)
    where
        P: Send + Sync + 'static,
    {
        let protocol_entity = self.world_mut().spawn(Protocol::default()).id();

        self.insert_resource(ProtocolEntity::<P> {
            _p: PhantomData,
            protocol_entity,
        });

        self.add_observer(insert_receive_protocol::<P>);
        self.add_observer(insert_receive_protocol_entity::<P>);
    }

    fn add_protocol_message<P, T>(&mut self)
    where
        P: Send + Sync + 'static,
        T: Send + Sync + 'static + DeserializeOwned,
    {
        let &ProtocolEntity::<P> {
            protocol_entity, ..
        } = self
            .world()
            .get_resource()
            .expect("Protocol not initialized");

        let mut protocol = self
            .world_mut()
            .get_mut::<Protocol>(protocol_entity)
            .unwrap();

        let id = protocol.messages.len();
        protocol.messages.push(TypeId::of::<T>());
        protocol.lookup.insert(TypeId::of::<T>(), id);

        deserialize::build_message::<T>(self);
    }

    fn include_protocol<P, O>(&mut self)
    where
        P: Send + Sync + 'static,
        O: Send + Sync + 'static,
    {
        let &ProtocolEntity::<P> {
            protocol_entity, ..
        } = self
            .world()
            .get_resource()
            .expect("Protocol not initialized");

        let &ProtocolEntity::<O> {
            protocol_entity: other_protocol_entity,
            ..
        } = self
            .world()
            .get_resource()
            .expect("Other protocol not initialized");

        let messages = self
            .world()
            .get::<Protocol>(other_protocol_entity)
            .unwrap()
            .messages
            .clone();

        let mut protocol = self
            .world_mut()
            .get_mut::<Protocol>(protocol_entity)
            .unwrap();

        for message in messages {
            let id = protocol.messages.len();
            protocol.messages.push(message);
            protocol.lookup.insert(message, id);
        }
    }
}

/// When a [`ConnectionOf`] is inserted and it's endpoint has a [`ReceiveProtocol<P>`],
/// insert a [`ReceiveProtocol<P>`] onto the connection.
fn insert_receive_protocol<P>(
    insert: On<Insert, ConnectionOf>,
    mut commands: Commands,
    connection_q: Query<&ConnectionOf>,
    endpoint_q: Query<(), With<ConnectionProtocol<P>>>,
) -> Result
where
    P: Send + Sync + 'static,
{
    let &ConnectionOf(endpoint_entity) = connection_q.get(insert.entity)?;

    if endpoint_q.contains(endpoint_entity) {
        commands
            .entity(insert.entity)
            .insert(ConnectionProtocol::<P>::default());
    }

    Ok(())
}

/// When a [`ConnectionProtocol<P>`] is inserted,
/// inserts a [`ConnectionProtocolEntity`] pointing to the [`Protocol`] [`Entity`] for protocol `P`.
fn insert_receive_protocol_entity<P>(
    insert: On<Insert, ConnectionProtocol<P>>,
    mut commands: Commands,
    connection_q: Query<(), With<ConnectionOf>>,
    protocol_entity: Res<ProtocolEntity<P>>,
) where
    P: Send + Sync + 'static,
{
    if !connection_q.contains(insert.entity) {
        return;
    }

    commands
        .entity(insert.entity)
        .insert(ConnectionProtocolEntity(**protocol_entity));
}
