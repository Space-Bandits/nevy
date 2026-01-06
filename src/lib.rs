use bevy::{
    app::PluginGroupBuilder,
    ecs::{intern::Interned, schedule::ScheduleLabel},
    prelude::*,
};

pub use nevy_transport as transport;

#[cfg(feature = "messages")]
pub use nevy_messages as messages;

pub mod prelude {
    pub use nevy_transport::prelude::*;

    #[cfg(feature = "messages")]
    pub use nevy_messages::prelude::*;

    pub use crate::NevyPlugins;
}

/// Adds plugins for all enabled features.
pub struct NevyPlugins {
    schedule: Interned<dyn ScheduleLabel>,
}

impl NevyPlugins {
    pub fn new(schedule: impl ScheduleLabel) -> Self {
        NevyPlugins {
            schedule: schedule.intern(),
        }
    }
}

impl Default for NevyPlugins {
    fn default() -> Self {
        Self::new(nevy_transport::DEFAULT_TRANSPORT_SCHEDULE)
    }
}

impl PluginGroup for NevyPlugins {
    fn build(self) -> PluginGroupBuilder {
        let builder = PluginGroupBuilder::start::<Self>()
            .add_group(nevy_transport::NevyTransportPlugins::new(self.schedule));

        #[cfg(feature = "messages")]
        let builder = builder.add(nevy_messages::NevyMessagesPlugin::new(self.schedule));

        builder
    }
}
