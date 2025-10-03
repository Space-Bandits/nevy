#![doc = include_str!("../readme.md")]

use bevy::{
    ecs::{intern::Interned, schedule::ScheduleLabel},
    prelude::*,
};

mod connection;
mod endpoint;

#[cfg(feature = "headers")]
pub(crate) mod headers;

#[cfg(feature = "messages")]
pub(crate) mod messages;

pub use quinn_proto::{self, Dir};

pub use connection::{
    Chunk, ConnectionState, ResetStreamError, StopStreamError, StreamEvent, StreamFinishError,
    StreamId, StreamReadError, StreamWriteError, StreamsExhausted, VarIntBoundsExceeded,
};

pub use endpoint::{
    ConnectionStatus, IncomingConnectionHandler, NoConnectionState, QuicConnection,
    QuicConnectionConfig, QuicEndpoint,
};

#[cfg(feature = "headers")]
pub use headers::{
    EndpointWithHeaderedConnections, HeaderedStreamState, NevyHeaderPlugin, RecvStreamHeaders,
    UpdateHeaders,
};

#[cfg(feature = "messages")]
pub use messages::{
    AddNetMessage, EndpointWithNetMessageConnections, NetMessageId, NetMessageReceiveHeader,
    NetMessageReceiveStreams, NevyNetMessagesPlugin, ReceivedNetMessages, UpdateNetMessageSet,
    senders::{
        AddSharedSender, LocalNetMessageSender, NetMessageSendHeader, NetMessageSendStreamState,
        NetMessageSender, SharedNetMessageSender,
    },
};

/// The schedule that nevy performs updates in by default
pub const DEFAULT_NEVY_SCHEDULE: PostUpdate = PostUpdate;

/// System set where quic endpoints are updated and packets are sent and received.
#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone, Copy)]
pub struct UpdateEndpoints;

/// Plugin which adds observers and update systems for quic endpoints and connections.
///
/// The default schedule for network updates is `PostUpdate`.
pub struct NevyPlugin {
    schedule: Interned<dyn ScheduleLabel>,
}

impl NevyPlugin {
    /// Creates a new plugin with updates happening in a specified schedule.
    pub fn new(schedule: impl ScheduleLabel) -> Self {
        NevyPlugin {
            schedule: schedule.intern(),
        }
    }
}

impl Default for NevyPlugin {
    fn default() -> Self {
        Self::new(DEFAULT_NEVY_SCHEDULE)
    }
}

impl Plugin for NevyPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(endpoint::inserted_connection_of_observer);
        app.add_observer(endpoint::removed_connection_of_observer);

        app.add_systems(
            self.schedule,
            endpoint::update_endpoints.in_set(UpdateEndpoints),
        );
    }
}

/// Relationship target for all [ConnectionOf]s for a [QuicEndpoint]
///
/// Use this component to get all the current connections on an endpoint.
#[derive(Component, Default)]
#[relationship_target(relationship = ConnectionOf)]
pub struct EndpointOf(Vec<Entity>);

impl EndpointOf {
    pub fn iter(&self) -> std::slice::Iter<'_, Entity> {
        self.0.iter()
    }
}

impl<'a> IntoIterator for &'a EndpointOf {
    type Item = &'a Entity;
    type IntoIter = std::slice::Iter<'a, Entity>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// This component represents a connection on a [QuicEndpoint].
///
/// This component will be inserted on a new entity when a connection is accepted.
///
/// Insert this component along with a [QuicConnectionConfig] to open a connection.
///
/// This component will not be removed when the connection is closed.
/// This is because there may still be data to be read from the connection.
/// You should add logic to either despawn connections that you are finished with
/// or reinsert this component to reopen the connection.
///
/// Removing this component or despawning the entity whilst a connection is open will
/// ungracefully drop the connection and the endpoint will stop responding to the peer.
#[derive(Component, Deref)]
#[component(immutable)]
#[relationship(relationship_target = EndpointOf)]
pub struct ConnectionOf(pub Entity);

/// This type implements [IncomingConnectionHandler] and will always accept incoming connections.
///
/// [Self::new] will create a `Box<dyn IncomingConnectionHandler>`.
pub struct AlwaysAcceptIncoming;

impl IncomingConnectionHandler for AlwaysAcceptIncoming {
    fn request(&mut self, _incoming: &quinn_proto::Incoming) -> bool {
        true
    }
}

impl AlwaysAcceptIncoming {
    pub fn new() -> Box<dyn IncomingConnectionHandler> {
        Box::new(AlwaysAcceptIncoming)
    }
}

/// This type implements [IncomingConnectionHandler] and will always reject incoming connections.
///
/// [Self::new] will create a `Box<dyn IncomingConnectionHandler>`.
pub struct AlwaysRejectIncoming;

impl IncomingConnectionHandler for AlwaysRejectIncoming {
    fn request(&mut self, _incoming: &quinn_proto::Incoming) -> bool {
        false
    }
}

impl AlwaysRejectIncoming {
    pub fn new() -> Box<dyn IncomingConnectionHandler> {
        Box::new(AlwaysRejectIncoming)
    }
}
