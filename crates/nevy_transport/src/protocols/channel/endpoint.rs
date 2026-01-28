//! Channel endpoint implementation.

use std::any::Any;

use bevy::{platform::collections::HashMap, prelude::*};
use crossbeam_channel::unbounded;

use crate::{
    Connection, ConnectionOf, ConnectionStatus, Endpoint, NoConnectionError, Transport,
};

use super::{
    connection::{ChannelConnectionContext, ChannelConnectionState, LinkConditionerConfig},
    registry::{ChannelMessage, ChannelRegistry, ConnectionRequest},
};

/// Configuration for initiating a channel connection.
///
/// Add this component along with [`ConnectionOf`] to connect to another endpoint.
#[derive(Component, Clone)]
pub struct ChannelConnectionConfig {
    /// The entity of the endpoint to connect to.
    pub remote_endpoint: Entity,
    /// Optional link conditioner to simulate network conditions on this connection.
    /// When `None`, messages pass through instantly with zero overhead.
    pub link_conditioner: Option<LinkConditionerConfig>,
}

/// Marker component for incoming channel connections.
///
/// When a remote endpoint initiates a connection, an entity with this component is spawned.
/// Either despawn the entity to reject the connection, or insert [`ConnectionOf`] pointing
/// to the endpoint entity to accept it.
#[derive(Component)]
pub struct IncomingChannelConnection {
    /// The endpoint entity this connection is for.
    pub endpoint_entity: Entity,
    /// The connection request data (taken when accepted).
    pub request: Option<ConnectionRequest>,
}

/// Channel endpoint implementation.
///
/// Provides local IPC transport using crossbeam channels for in-process communication.
pub struct ChannelEndpoint {
    /// Map from connection entity to connection state.
    connections: HashMap<Entity, ChannelConnectionState>,
}

impl ChannelEndpoint {
    /// Create a new channel endpoint.
    ///
    /// The endpoint must be registered with the [`ChannelRegistry`] resource.
    pub fn new() -> Endpoint {
        Endpoint::new(Self {
            connections: HashMap::new(),
        })
    }

    /// Internal: add a connection state.
    pub(crate) fn add_connection(&mut self, entity: Entity, state: ChannelConnectionState) {
        self.connections.insert(entity, state);
    }

    /// Internal: remove a connection.
    pub(crate) fn remove_connection(&mut self, entity: Entity) {
        self.connections.remove(&entity);
    }

}

impl Default for ChannelEndpoint {
    fn default() -> Self {
        Self {
            connections: HashMap::new(),
        }
    }
}

impl Transport for ChannelEndpoint {
    fn as_any<'a>(&'a mut self) -> &'a mut dyn Any {
        self
    }

    fn get_connection<'a>(
        &'a mut self,
        connection: Entity,
    ) -> Result<Connection<'a>, NoConnectionError> {
        let state = self.connections.get_mut(&connection).ok_or(NoConnectionError)?;

        // Poll for new messages
        state.poll_messages();

        Ok(Connection(Box::new(ChannelConnectionContext { state })))
    }
}

pub(super) fn update_endpoints(
    mut commands: Commands,
    mut endpoint_q: Query<(Entity, &mut Endpoint)>,
    registry: Res<ChannelRegistry>,
) {
    for (endpoint_entity, mut endpoint) in endpoint_q.iter_mut() {
        let Some(endpoint) = endpoint.as_transport::<ChannelEndpoint>() else {
            continue;
        };

        // Accept incoming connection requests from registry
        while let Some(request) = registry.try_recv_incoming(endpoint_entity) {
            commands.spawn(IncomingChannelConnection {
                endpoint_entity,
                request: Some(request),
            });
        }

        // Update connection statuses based on close requests
        let mut to_close = Vec::new();
        for (&entity, state) in endpoint.connections.iter_mut() {
            state.poll_messages();
            if state.close_requested {
                to_close.push(entity);
            }
        }

        for entity in to_close {
            commands.entity(entity).insert(ConnectionStatus::Closed);
        }
    }
}

pub(super) fn create_connections(
    insert: On<Insert, ConnectionOf>,
    mut commands: Commands,
    mut endpoint_q: Query<&mut Endpoint>,
    mut connection_q: Query<(
        &ConnectionOf,
        Option<&ChannelConnectionConfig>,
        Option<&mut IncomingChannelConnection>,
    )>,
    registry: Res<ChannelRegistry>,
) -> Result {
    let connection_entity = insert.entity;

    let (connection_of, config, incoming) = connection_q.get_mut(connection_entity)?;

    let mut endpoint = endpoint_q.get_mut(**connection_of)?;

    let Some(endpoint) = endpoint.as_transport::<ChannelEndpoint>() else {
        return Ok(());
    };

    if let Some(mut incoming) = incoming {
        // Accept incoming connection
        commands
            .entity(connection_entity)
            .remove::<IncomingChannelConnection>();

        let Some(request) = incoming.request.take() else {
            log::error!("User tried to accept an incoming channel connection twice.");
            return Ok(());
        };

        // Create connection state using the channels from the request.
        // Incoming connections don't have a conditioner by default.
        let state = ChannelConnectionState::new(
            request.to_remote_tx,
            request.from_remote_rx,
            None,
        );

        endpoint.add_connection(connection_entity, state);

        commands
            .entity(connection_entity)
            .insert(ConnectionStatus::Established);
    } else {
        // Initiate outgoing connection
        let connection_config = config.ok_or_else(|| {
            commands.entity(connection_entity).remove::<ConnectionOf>();
            format!(
                "No ChannelConnectionConfig provided for connection {}. Removing `ConnectionOf`",
                connection_entity
            )
        })?;

        // Get the remote endpoint's incoming channel
        let Some(remote_tx) = registry.get_incoming_tx(connection_config.remote_endpoint) else {
            log::warn!(
                "Remote endpoint {} not found in registry",
                connection_config.remote_endpoint
            );
            commands
                .entity(connection_entity)
                .insert(ConnectionStatus::Failed);
            return Ok(());
        };

        // Create channel pair for this connection
        let (to_remote_tx, from_local_rx) = unbounded::<ChannelMessage>();
        let (to_local_tx, from_remote_rx) = unbounded::<ChannelMessage>();

        // Send connection request to remote endpoint
        let request = ConnectionRequest {
            from_endpoint: **connection_of,
            from_connection: connection_entity,
            to_remote_tx: to_local_tx,
            from_remote_rx: from_local_rx,
        };

        if remote_tx.try_send(request).is_err() {
            log::warn!(
                "Failed to send connection request to remote endpoint {}",
                connection_config.remote_endpoint
            );
            commands
                .entity(connection_entity)
                .insert(ConnectionStatus::Failed);
            return Ok(());
        }

        // Create local connection state
        let state = ChannelConnectionState::new(
            to_remote_tx,
            from_remote_rx,
            connection_config.link_conditioner.clone(),
        );
        endpoint.add_connection(connection_entity, state);

        // Channel connections are established immediately
        commands
            .entity(connection_entity)
            .insert(ConnectionStatus::Established);
    }

    Ok(())
}

pub(super) fn remove_connections(
    replace: On<Replace, ConnectionOf>,
    connection_q: Query<&ConnectionOf>,
    mut endpoint_q: Query<&mut Endpoint>,
) -> Result {
    let connection_entity = replace.entity;

    let connection_of = connection_q.get(connection_entity)?;

    let Ok(mut endpoint) = endpoint_q.get_mut(**connection_of) else {
        return Ok(());
    };

    let Some(endpoint) = endpoint.as_transport::<ChannelEndpoint>() else {
        return Ok(());
    };

    endpoint.remove_connection(connection_entity);

    Ok(())
}

pub(super) fn refuse_connections(
    replace: On<Replace, IncomingChannelConnection>,
    mut connection_q: Query<&mut IncomingChannelConnection>,
) -> Result {
    let connection_entity = replace.entity;

    let mut incoming_component = connection_q.get_mut(connection_entity)?;

    // Just drop the request - this implicitly refuses the connection
    // as the channels will be disconnected
    incoming_component.request.take();

    Ok(())
}

pub(super) fn register_endpoint(
    insert: On<Insert, Endpoint>,
    mut registry: ResMut<ChannelRegistry>,
) {
    let entity = insert.entity;

    // Register all endpoints. The create_connections observer will filter by type.
    // This is safe because non-channel endpoints simply won't use the registry.
    registry.register(entity);
}

pub(super) fn unregister_endpoint(
    replace: On<Replace, Endpoint>,
    mut registry: ResMut<ChannelRegistry>,
) {
    let entity = replace.entity;
    registry.unregister(entity);
}
