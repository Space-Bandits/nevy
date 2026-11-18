use bevy::{
    app::PluginGroupBuilder,
    ecs::{intern::Interned, schedule::ScheduleLabel},
    prelude::*,
};

pub use nevy_transport as transport;

#[cfg(feature = "messages")]
pub use nevy_messages as messages;

#[cfg(feature = "stream_headers")]
use nevy_transport::headers::StreamHeader;

pub mod prelude {
    pub use nevy_transport::prelude::*;

    #[cfg(feature = "messages")]
    pub use nevy_messages::prelude::*;

    pub use crate::NevyPlugins;
}

/// Adds plugins for all enabled features.
pub struct NevyPlugins {
    schedule: Interned<dyn ScheduleLabel>,
    #[cfg(feature = "stream_headers")]
    stream_headers: Option<StreamHeaderConfig>,
}

#[cfg(feature = "stream_headers")]
struct StreamHeaderConfig {
    #[cfg_attr(not(feature = "messages"), allow(dead_code))]
    messages: StreamHeader,
}

impl NevyPlugins {
    pub fn new(schedule: impl ScheduleLabel) -> Self {
        NevyPlugins {
            schedule: schedule.intern(),
            #[cfg(feature = "stream_headers")]
            stream_headers: None,
        }
    }

    /// Adds [`StreamHeadersPlugin`] and sends messages only on streams with `messages_header`.
    ///
    /// [`StreamHeadersPlugin`]: nevy_transport::headers::StreamHeadersPlugin
    #[cfg(feature = "stream_headers")]
    pub fn with_stream_headers(mut self, messages_header: impl Into<StreamHeader>) -> Self {
        self.stream_headers = Some(StreamHeaderConfig {
            messages: messages_header.into(),
        });
        self
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

        #[cfg(feature = "stream_headers")]
        let builder = match self.stream_headers {
            Some(_) => builder.add(nevy_transport::headers::StreamHeadersPlugin::new(
                self.schedule,
            )),
            None => builder,
        };

        #[cfg(feature = "messages")]
        let builder = {
            let messages = nevy_messages::NevyMessagesPlugin::new(self.schedule);

            #[cfg(feature = "stream_headers")]
            let messages = match &self.stream_headers {
                Some(config) => messages.with_header(config.messages),
                None => messages,
            };

            builder.add(messages)
        };

        builder
    }
}
