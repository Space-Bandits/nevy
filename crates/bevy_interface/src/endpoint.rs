use bevy::{
    ecs::error::debug,
    platform::collections::{hash_map::Entry, HashMap},
    prelude::*,
};
use transport_interface::*;

use crate::{
    connections::BevyConnectionMut, description::Description, Connected, Disconnected,
    MismatchedType,
};

/// the component that holds state and represents a networking endpoint
///
/// use the [Connections] system parameter to manage connections
#[derive(Component)]
pub struct BevyEndpoint {
    /// dynamic dispatch is necessary here so that endpoints can be queried but still type erased
    ///
    /// values will be a [BevyEndpointState<E>] of their endpoint type
    state: Box<dyn BevyEndpointType>,
}

trait BevyEndpointType: Send + Sync {
    fn update(&mut self, endpoint_entity: Entity, params: &mut UpdateHandlerParams);

    fn connect(
        &mut self,
        commands: &mut Commands,
        endpoint_entity: Entity,
        connect_info: Description,
    ) -> Result<Option<Entity>, MismatchedType>;

    fn connection_mut<'c>(&'c mut self, connection_entity: Entity)
        -> Option<BevyConnectionMut<'c>>;
}

struct BevyEndpointState<E: Endpoint>
where
    E: Send + Sync,
    E::ConnectionId: Send + Sync,
{
    pub(crate) endpoint: E,
    /// a two way map from the endpoint connection ids to entities
    ///
    /// entities are used as connection ids so that the application doesn't need to specify generics
    connections: ConnectionMap<E::ConnectionId>,
}

/// a two way map of connection ids to entities
///
/// provides methods to hold the two way map invariant
struct ConnectionMap<C: std::hash::Hash + Eq + Copy> {
    connection_entities: HashMap<C, Entity>,
    connection_ids: HashMap<Entity, C>,
}

/// marker component for connections
///
/// will exist on all [BevyConnectionState]s,
/// but has no generic so it can be queried without that type info
#[derive(Component)]
pub struct BevyConnection;

/// system params used by [UpdateHandler]
#[derive(bevy::ecs::system::SystemParam)]
pub(crate) struct UpdateHandlerParams<'w, 's> {
    commands: Commands<'w, 's>,
    connected_w: EventWriter<'w, Connected>,
    disconnected_w: EventWriter<'w, Disconnected>,
}

/// the endpoint event handler for updating endpoints in bevy
struct UpdateHandler<'a, 'w, 's, E: Endpoint> {
    params: &'a mut UpdateHandlerParams<'w, 's>,
    accept_inoming: bool,
    endpoint_entity: Entity,
    connections: &'a mut ConnectionMap<E::ConnectionId>,
}

#[derive(bevy::ecs::system::SystemParam)]
pub struct Connections<'w, 's> {
    commands: Commands<'w, 's>,
    endpoint_q: Query<'w, 's, &'static mut BevyEndpoint>,
    connection_q: Query<'w, 's, &'static ChildOf, With<BevyConnection>>,
}

impl BevyEndpoint {
    pub fn new<E: Endpoint>(endpoint: E) -> Self
    where
        E: Send + Sync + 'static,
        E::ConnectionId: Send + Sync,
        for<'a> <E::Connection<'a> as ConnectionMut<'a>>::StreamType: Send + Sync,
    {
        BevyEndpoint {
            state: Box::new(BevyEndpointState {
                endpoint,
                connections: ConnectionMap::new(),
            }),
        }
    }

    pub fn connection_mut(&mut self, connection_entity: Entity) -> Option<BevyConnectionMut> {
        self.state.connection_mut(connection_entity)
    }
}

impl<E: Endpoint> BevyEndpointType for BevyEndpointState<E>
where
    E: Send + Sync + 'static,
    E::ConnectionId: Send + Sync,
    for<'a> <E::Connection<'a> as ConnectionMut<'a>>::StreamType: Send + Sync,
{
    fn update(&mut self, endpoint_entity: Entity, params: &mut UpdateHandlerParams) {
        self.endpoint.update(&mut UpdateHandler {
            params,
            accept_inoming: true, // TODO: add api
            endpoint_entity,
            connections: &mut self.connections,
        });
    }

    fn connect(
        &mut self,
        commands: &mut Commands,
        endpoint_entity: Entity,
        connect_info: Description,
    ) -> Result<Option<Entity>, MismatchedType> {
        let connect_info = connect_info.downcast()?;

        let Some((connection_id, _)) = self.endpoint.connect(connect_info) else {
            return Ok(None);
        };

        let entity = commands
            .spawn((BevyConnection, ChildOf(endpoint_entity)))
            .id();

        if self.connections.insert(connection_id, entity) {
            panic!(
                "got duplicate connection id from endpoint {:?} \"{}\"",
                endpoint_entity,
                std::any::type_name::<E>()
            );
        }

        Ok(Some(entity))
    }

    fn connection_mut<'c>(
        &'c mut self,
        connection_entity: Entity,
    ) -> Option<BevyConnectionMut<'c>> {
        let connection_id = self.connections.get_connection_id(connection_entity)?;

        let Some(connection_mut) = self.endpoint.connection_mut(connection_id) else {
            return None;
        };

        let connection_mut = BevyConnectionMut::new(connection_mut);

        Some(connection_mut)
    }
}

impl<C: std::hash::Hash + Eq + Copy> ConnectionMap<C> {
    fn new() -> Self {
        ConnectionMap {
            connection_ids: HashMap::new(),
            connection_entities: HashMap::new(),
        }
    }

    /// attempts to insert a new map
    ///
    /// returns `true` if an entry for either key exists and the operation failed
    fn insert(&mut self, connection_id: C, entity: Entity) -> bool {
        match (
            self.connection_entities.entry(connection_id),
            self.connection_ids.entry(entity),
        ) {
            (Entry::Vacant(connection_entry), Entry::Vacant(entity_entry)) => {
                connection_entry.insert(entity);
                entity_entry.insert(connection_id);

                false
            }
            _ => true,
        }
    }

    /// attempts to remove from the map from a `connection_id`
    fn remove_connection(&mut self, connection_id: C) -> Option<Entity> {
        let Entry::Occupied(connection_entry) = self.connection_entities.entry(connection_id)
        else {
            return None;
        };
        let entity = connection_entry.remove();

        let Entry::Occupied(entity_entry) = self.connection_ids.entry(entity) else {
            unreachable!("A matching entry should always exist in the other map");
        };
        entity_entry.remove();

        Some(entity)
    }

    fn get_connection_id(&self, entity: Entity) -> Option<C> {
        self.connection_ids.get(&entity).copied()
    }

    fn get_connection_entity(&self, connection_id: C) -> Option<Entity> {
        self.connection_entities.get(&connection_id).copied()
    }
}

impl<'a, 'w, 's, E: Endpoint> EndpointEventHandler<E> for UpdateHandler<'a, 'w, 's, E>
where
    E::ConnectionId: Send + Sync,
{
    fn connection_request<'i>(
        &mut self,
        _request: <E as Endpoint>::IncomingConnectionInfo<'i>,
    ) -> bool {
        self.accept_inoming
    }

    fn connected(&mut self, connection_id: <E as Endpoint>::ConnectionId) {
        let connection_entity = match self.connections.get_connection_entity(connection_id) {
            Some(connection_entity) => connection_entity,
            None => {
                let connection_entity = self
                    .params
                    .commands
                    .spawn((BevyConnection, ChildOf(self.endpoint_entity)))
                    .id();

                self.connections.insert(connection_id, connection_entity);

                connection_entity
            }
        };

        self.params.connected_w.write(Connected {
            endpoint_entity: self.endpoint_entity,
            connection_entity,
        });
    }

    fn disconnected(&mut self, connection_id: <E as Endpoint>::ConnectionId) {
        if let Some(connection_entity) = self.connections.remove_connection(connection_id) {
            self.params.commands.entity(connection_entity).despawn();

            self.params.disconnected_w.write(Disconnected {
                endpoint_entity: self.endpoint_entity,
                connection_entity,
            });
        }
    }
}

#[derive(Debug)]
pub enum ConnectError {
    MismatchedEndpointType(MismatchedType),
    InvalidEntity,
}

impl<'w, 's> Connections<'w, 's> {
    pub fn connect(
        &mut self,
        endpoint_entity: Entity,
        connect_info: Description,
    ) -> Result<Option<Entity>, ConnectError> {
        let Ok(mut endpoint) = self.endpoint_q.get_mut(endpoint_entity) else {
            return Err(ConnectError::InvalidEntity);
        };

        endpoint
            .state
            .connect(&mut self.commands, endpoint_entity, connect_info)
            .map_err(|err| ConnectError::MismatchedEndpointType(err))
    }

    /// gets the [BevyEndpoint] component of a connection's parent mutably
    pub fn connection_endpoint_mut(
        &mut self,
        connection_entity: Entity,
    ) -> Result<Option<Mut<BevyEndpoint>>, BevyError> {
        let Ok(parent) = self.connection_q.get(connection_entity) else {
            return Ok(None);
        };

        let connection_parent = parent.parent();

        let Ok(endpoint) = self.endpoint_q.get_mut(connection_parent) else {
            return Err(format!(
                "connection entity {:?}'s parent {:?} wasn't an endpoint",
                connection_entity, connection_parent
            )
            .into());
        };

        Ok(Some(endpoint))
    }
}

pub(crate) fn update_endpoints(
    mut params: UpdateHandlerParams,
    mut endpoint_q: Query<(Entity, &mut BevyEndpoint)>,
) {
    for (endpoint_entity, mut endpoint) in endpoint_q.iter_mut() {
        endpoint.state.update(endpoint_entity, &mut params);
    }
}
