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

pub use quinn_proto;

mod connection;
mod endpoint;

pub use connection::{
    Chunk, ConnectionState, ResetStreamError, StopStreamError, StreamEvent, StreamFinishError,
    StreamId, StreamReadError, StreamWriteError, VarIntBoundsExceeded,
};

pub use endpoint::{
    ConnectionStatus, IncomingConnectionHandler, NoConnectionState, QuicConnection,
    QuicConnectionConfig, QuicEndpoint,
};

/// System set where quic endpoints are updated and packets are sent and received.
#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone, Copy)]
pub struct UpdateEndpoints;

pub struct NevyPlugin {
    schedule: Interned<dyn ScheduleLabel>,
}

impl NevyPlugin {
    pub fn new(schedule: impl ScheduleLabel) -> Self {
        NevyPlugin {
            schedule: schedule.intern(),
        }
    }
}

impl Default for NevyPlugin {
    fn default() -> Self {
        Self::new(PostUpdate)
    }
}

impl Plugin for NevyPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(endpoint::new_connection_observer);
        app.add_observer(endpoint::removed_connection_observer);

        app.add_systems(
            self.schedule,
            endpoint::update_endpoints.in_set(UpdateEndpoints),
        );
    }
}
#[derive(Component, Default)]
#[relationship_target(relationship = ConnectionOf)]
pub struct EndpointOf(Vec<Entity>);

/// This component represents a connection on a [QuicEndpoint].
///
/// Insert this component to open a connection.
#[derive(Deref)]
pub struct ConnectionOf(pub Entity);

impl Component for ConnectionOf {
    const STORAGE_TYPE: StorageType = StorageType::SparseSet;

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

/// Event that is triggered when a new [ConnectionOf] component is inserted.
/// The target is the associated endpoint.
#[derive(Event)]
pub struct NewConnectionOf(pub Entity);

/// Event that is triggered when a [ConnectionOf] component is removed.
/// The target is the associated endpoint.
#[derive(Event)]
pub struct RemovedConnectionOf(pub Entity);

/// The directionality of a stream
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Bi,
    Uni,
}

impl From<quinn_proto::Dir> for Direction {
    fn from(dir: quinn_proto::Dir) -> Self {
        match dir {
            quinn_proto::Dir::Bi => Direction::Bi,
            quinn_proto::Dir::Uni => Direction::Uni,
        }
    }
}

impl From<Direction> for quinn_proto::Dir {
    fn from(direction: Direction) -> Self {
        match direction {
            Direction::Bi => quinn_proto::Dir::Bi,
            Direction::Uni => quinn_proto::Dir::Uni,
        }
    }
}

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
