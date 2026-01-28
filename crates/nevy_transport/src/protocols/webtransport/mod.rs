//! Native WebTransport protocol implementation.
//!
//! This module provides WebTransport support on top of QUIC using HTTP/3 framing.
//! WebTransport enables bidirectional communication between a client and server
//! with support for multiple streams and unreliable datagrams.

use bevy::{
    ecs::{intern::Interned, schedule::ScheduleLabel},
    prelude::*,
};

use crate::{DEFAULT_TRANSPORT_SCHEDULE, TransportUpdateSystems};

pub mod connection;
pub mod endpoint;
mod h3;
mod session;

pub use connection::{WebTransportConnectionContext, WebTransportStreamId};
pub use endpoint::{
    IncomingWebTransportConnection, WebTransportConnectionClosedReason, WebTransportConnectionConfig,
    WebTransportConnectionFailedReason, WebTransportEndpoint,
};
pub use session::SessionState;

/// Plugin that adds WebTransport protocol support.
///
/// This plugin registers the systems needed to handle WebTransport connections
/// on top of QUIC with HTTP/3 framing.
pub struct WebTransportPlugin {
    schedule: Interned<dyn ScheduleLabel>,
}

impl WebTransportPlugin {
    /// Creates a new WebTransport plugin that runs in the specified schedule.
    pub fn new(schedule: impl ScheduleLabel) -> Self {
        Self {
            schedule: schedule.intern(),
        }
    }
}

impl Default for WebTransportPlugin {
    fn default() -> Self {
        Self::new(DEFAULT_TRANSPORT_SCHEDULE)
    }
}

impl Plugin for WebTransportPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(endpoint::create_connections);
        app.add_observer(endpoint::remove_connections);
        app.add_observer(endpoint::refuse_connections);

        app.add_systems(
            self.schedule,
            endpoint::update_endpoints.in_set(TransportUpdateSystems),
        );
    }
}
