//! Local IPC transport using crossbeam channels.
//!
//! This module provides a channel-based transport for in-process communication.
//! Useful for:
//! - Testing without network overhead
//! - Local multiplayer (split-screen games)
//! - Single-process server+client setups
//! - Communication between ECS worlds/apps

use bevy::{
    ecs::{intern::Interned, schedule::ScheduleLabel},
    prelude::*,
};

use crate::{DEFAULT_TRANSPORT_SCHEDULE, TransportUpdateSystems};

pub mod connection;
pub mod endpoint;
pub mod registry;

pub use connection::{ChannelConnectionContext, ChannelStreamId, LinkConditionerConfig};
pub use endpoint::{ChannelConnectionConfig, ChannelEndpoint, IncomingChannelConnection};
pub use registry::{ChannelMessage, ChannelRegistry};

/// Plugin that adds channel transport functionality.
///
/// Systems are in the [`DEFAULT_TRANSPORT_SCHEDULE`] unless constructed with [`ChannelTransportPlugin::new`].
pub struct ChannelTransportPlugin {
    schedule: Interned<dyn ScheduleLabel>,
}

impl ChannelTransportPlugin {
    pub fn new(schedule: impl ScheduleLabel) -> Self {
        ChannelTransportPlugin {
            schedule: schedule.intern(),
        }
    }
}

impl Default for ChannelTransportPlugin {
    fn default() -> Self {
        Self::new(DEFAULT_TRANSPORT_SCHEDULE)
    }
}

impl Plugin for ChannelTransportPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ChannelRegistry>();

        app.add_observer(endpoint::create_connections);
        app.add_observer(endpoint::remove_connections);
        app.add_observer(endpoint::refuse_connections);
        app.add_observer(endpoint::register_endpoint);
        app.add_observer(endpoint::unregister_endpoint);

        app.add_systems(
            self.schedule,
            endpoint::update_endpoints.in_set(TransportUpdateSystems),
        );
    }
}
