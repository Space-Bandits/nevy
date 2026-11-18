use bevy::{
    ecs::{intern::Interned, schedule::ScheduleLabel},
    prelude::*,
};
use nevy_transport::{DEFAULT_TRANSPORT_SCHEDULE, TransportUpdateSystems};

#[cfg(feature = "stream_headers")]
use nevy_transport::headers::{RegisterStreamHeader, StreamHeader, StreamHeaderSystems};

pub mod deserialize;
pub mod prelude;
pub mod protocol;
pub mod reader;
pub mod writer;

#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone, Copy)]
pub enum MessageSystems {
    ReadStreams,
    DeserializeMessages,
}

/// Which incoming streams carry messages.
#[derive(Clone, Copy, Debug)]
pub enum MessageStreams {
    /// Every stream a connection accepts carries messages.
    AcceptAll,
    /// Only streams with this header carry messages, and every message stream is opened with it.
    ///
    /// Requires [`StreamHeadersPlugin`] to be added.
    ///
    /// [`StreamHeadersPlugin`]: nevy_transport::headers::StreamHeadersPlugin
    #[cfg(feature = "stream_headers")]
    Header(StreamHeader),
}

pub struct NevyMessagesPlugin {
    schedule: Interned<dyn ScheduleLabel>,
    streams: MessageStreams,
}

impl NevyMessagesPlugin {
    pub fn new(schedule: impl ScheduleLabel) -> Self {
        NevyMessagesPlugin {
            schedule: schedule.intern(),
            streams: MessageStreams::AcceptAll,
        }
    }

    /// Sends and receives messages only on streams with this header.
    /// See [`MessageStreams::Header`].
    #[cfg(feature = "stream_headers")]
    pub fn with_header(mut self, header: impl Into<StreamHeader>) -> Self {
        self.streams = MessageStreams::Header(header.into());
        self
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

        match self.streams {
            MessageStreams::AcceptAll => {
                app.add_systems(
                    self.schedule,
                    (reader::accept_streams, reader::read_streams)
                        .chain()
                        .in_set(MessageSystems::ReadStreams),
                );
            }
            #[cfg(feature = "stream_headers")]
            MessageStreams::Header(header) => {
                app.register_stream_header(header);
                app.insert_resource(writer::MessageStreamHeader(header));

                app.configure_sets(
                    self.schedule,
                    StreamHeaderSystems.before(MessageSystems::ReadStreams),
                );

                app.add_systems(
                    self.schedule,
                    (reader::accept_headered_streams, reader::read_streams)
                        .chain()
                        .in_set(MessageSystems::ReadStreams),
                );
            }
        }
    }
}

#[derive(Resource, Deref)]
struct NevyMessagesSchedule(Interned<dyn ScheduleLabel>);
