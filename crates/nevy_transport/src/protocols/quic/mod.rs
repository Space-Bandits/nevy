use crate::{DEFAULT_TRANSPORT_SCHEDULE, TransportUpdateSystems};
use bevy::{
    ecs::{intern::Interned, schedule::ScheduleLabel},
    prelude::*,
};

pub use quinn_proto;

pub mod connection;
pub mod endpoint;

/// Adds QUIC transport functionality to the app.
///
/// Systems are in the [`DEFAULT_TRANSPORT_SCHEDULE`]
/// unless constructed with [`QuicTransportPlugin::new`].
pub struct QuicTransportPlugin {
    schedule: Interned<dyn ScheduleLabel>,
}

impl QuicTransportPlugin {
    pub fn new(schedule: impl ScheduleLabel) -> Self {
        QuicTransportPlugin {
            schedule: schedule.intern(),
        }
    }
}

impl Default for QuicTransportPlugin {
    fn default() -> Self {
        Self::new(DEFAULT_TRANSPORT_SCHEDULE)
    }
}

impl Plugin for QuicTransportPlugin {
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

pub(crate) fn udp_transmit<'a>(
    transmit: &'a quinn_proto::Transmit,
    buffer: &'a [u8],
) -> quinn_udp::Transmit<'a> {
    quinn_udp::Transmit {
        destination: transmit.destination,
        ecn: transmit.ecn.map(|ecn| match ecn {
            quinn_proto::EcnCodepoint::Ect0 => quinn_udp::EcnCodepoint::Ect0,
            quinn_proto::EcnCodepoint::Ect1 => quinn_udp::EcnCodepoint::Ect1,
            quinn_proto::EcnCodepoint::Ce => quinn_udp::EcnCodepoint::Ce,
        }),
        contents: &buffer[0..transmit.size],
        segment_size: transmit.segment_size,
        src_ip: transmit.src_ip,
    }
}
