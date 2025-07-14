use bevy::{
    ecs::{
        component::{ComponentHook, HookContext, StorageType},
        intern::Interned,
        relationship::Relationship,
        schedule::ScheduleLabel,
        world::DeferredWorld,
    },
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
    senders::{AddSharedSender, LocalMessageSender, MessageSendStreamState, SharedMessageSender},
    AddMessage, EndpointWithMessageConnections, MessageId, MessageRecvStreams, MessageStreamHeader,
    NevyMessagesPlugin, ReceivedMessages, UpdateMessageSet,
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
/// Insert this component along with a [QuicConnectionConfig] to open a connection.
#[derive(Deref)]
pub struct ConnectionOf(pub Entity);

impl Component for ConnectionOf {
    const STORAGE_TYPE: StorageType = StorageType::Table;

    type Mutability = bevy::ecs::component::Immutable;

    fn on_insert() -> Option<ComponentHook> {
        Some(|mut world: DeferredWorld, hook_context: HookContext| {
            <Self as Relationship>::on_insert(world.reborrow(), hook_context);

            let target_entity = world.entity(hook_context.entity).get::<Self>().unwrap().0;

            world.trigger_targets(NewConnectionOf(hook_context.entity), target_entity);
        })
    }

    fn on_replace() -> Option<ComponentHook> {
        Some(<Self as Relationship>::on_replace)
    }

    fn on_remove() -> Option<ComponentHook> {
        Some(|mut world: DeferredWorld, hook_context: HookContext| {
            let target_entity = world.entity(hook_context.entity).get::<Self>().unwrap().0;

            world.trigger_targets(RemovedConnectionOf(hook_context.entity), target_entity);
        })
    }
}

impl Relationship for ConnectionOf {
    type RelationshipTarget = EndpointOf;

    fn get(&self) -> Entity {
        self.0
    }

    fn from(entity: Entity) -> Self {
        ConnectionOf(entity)
    }
}

/// Observer event that is triggered when a new [ConnectionOf] component is inserted.
/// The target entity is the associated endpoint.
#[derive(Event)]
pub struct NewConnectionOf(pub Entity);

/// Observer event that is triggered when a [ConnectionOf] component is removed.
/// The target entity is the associated endpoint.
#[derive(Event)]
pub struct RemovedConnectionOf(pub Entity);

/// This type implements [IncomingConnectionHandler] and will always accept incoming connections.
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
