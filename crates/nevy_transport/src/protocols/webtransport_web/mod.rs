//! Browser WebTransport protocol implementation using web_sys.
//!
//! This module provides WebTransport support for WASM targets using the browser's
//! native WebTransport API via web_sys bindings.

use bevy::{
    ecs::{intern::Interned, schedule::ScheduleLabel},
    prelude::*,
};

use crate::{DEFAULT_TRANSPORT_SCHEDULE, TransportUpdateSystems};

pub mod connection;
pub mod endpoint;
mod promise;

pub use connection::{WebTransportWebConnectionContext, WebTransportWebStreamId};
pub use endpoint::{WebTransportWebConfig, WebTransportWebEndpoint};

/// Plugin that adds browser WebTransport protocol support.
///
/// This plugin registers the systems needed to handle WebTransport connections
/// in the browser using the native WebTransport API.
pub struct WebTransportWebPlugin {
    schedule: Interned<dyn ScheduleLabel>,
}

impl WebTransportWebPlugin {
    /// Creates a new browser WebTransport plugin that runs in the specified schedule.
    pub fn new(schedule: impl ScheduleLabel) -> Self {
        Self {
            schedule: schedule.intern(),
        }
    }
}

impl Default for WebTransportWebPlugin {
    fn default() -> Self {
        Self::new(DEFAULT_TRANSPORT_SCHEDULE)
    }
}

impl Plugin for WebTransportWebPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(endpoint::create_connections);
        app.add_observer(endpoint::remove_connections);

        app.add_systems(
            self.schedule,
            endpoint::update_endpoints.in_set(TransportUpdateSystems),
        );
    }
}
