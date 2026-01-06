use bevy::{
    ecs::{intern::Interned, schedule::ScheduleLabel},
    prelude::*,
};
use nevy_transport::{DEFAULT_TRANSPORT_SCHEDULE, TransportUpdateSystems};

pub mod deserialize;
pub mod prelude;
pub mod protocol;
pub mod reader;
pub mod varint;
pub mod writer;

#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone, Copy)]
pub enum MessageSystems {
    ReadStreams,
    DeserializeMessages,
}

pub struct NevyMessagesPlugin {
    schedule: Interned<dyn ScheduleLabel>,
}

impl NevyMessagesPlugin {
    pub fn new(schedule: impl ScheduleLabel) -> Self {
        NevyMessagesPlugin {
            schedule: schedule.intern(),
        }
    }
}

impl Default for NevyMessagesPlugin {
    fn default() -> Self {
        Self::new(DEFAULT_TRANSPORT_SCHEDULE)
    }
}

impl Plugin for NevyMessagesPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(NevyMessagesSchedule(self.schedule));

        app.configure_sets(
            self.schedule,
            (
                TransportUpdateSystems,
                MessageSystems::ReadStreams,
                MessageSystems::DeserializeMessages,
            )
                .chain(),
        );

        app.add_systems(
            self.schedule,
            (reader::accept_streams, reader::read_streams)
                .chain()
                .in_set(MessageSystems::ReadStreams),
        );
    }
}

#[derive(Resource, Deref)]
struct NevyMessagesSchedule(Interned<dyn ScheduleLabel>);

pub fn bincode_config() -> bincode::config::Configuration {
    bincode::config::standard()
}
