use std::any::Any;

use bevy::{
    app::PluginGroupBuilder,
    ecs::{intern::Interned, schedule::ScheduleLabel},
    prelude::*,
};
use bytes::Bytes;
use thiserror::Error;

pub mod prelude;
pub mod protocols;

/// The schedule where implementations place their update systems by default.
pub const DEFAULT_TRANSPORT_SCHEDULE: PostUpdate = PostUpdate;

/// The system set where implementations place their update systems.
#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone, Copy)]
pub struct TransportUpdateSystems;

/// Adds plugins for all transport protocols that have been feature enabled.
///
/// You can also add the same plugins manually.
///
/// Systems are in the [`DEFAULT_TRANSPORT_SCHEDULE`]
/// unless constructed with [`NevyTransportPlugins::new`].
pub struct NevyTransportPlugins {
    schedule: Interned<dyn ScheduleLabel>,
}

impl NevyTransportPlugins {
    pub fn new(schedule: impl ScheduleLabel) -> Self {
        NevyTransportPlugins {
            schedule: schedule.intern(),
        }
    }
}

impl Default for NevyTransportPlugins {
    fn default() -> Self {
        Self::new(DEFAULT_TRANSPORT_SCHEDULE)
    }
}

impl PluginGroup for NevyTransportPlugins {
    fn build(self) -> PluginGroupBuilder {
        let builder = PluginGroupBuilder::start::<Self>();

        #[cfg(feature = "quic")]
        let builder = builder.add(protocols::quic::QuicTransportPlugin::new(self.schedule));

        builder
    }
}

/// [`RelationshipTarget`] of [`ComponentOf`].
///
/// Stores all connection entities on this endpoint.
#[derive(Component, Deref)]
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

/// [`Relationship`](bevy::ecs::relationship::Relationship) to [`EndpointOf`].
///
/// The lifecycle of this component is the lifecycle of the connection on it's endpoint.
///
/// The user is the one who inserts this component,
/// which is how connections are both initiated and accepted.
/// What other components are present, depending on the endpoint implementation,
/// is what controls how the connection is created.
#[derive(Component, Deref)]
#[component(immutable)]
#[relationship(relationship_target = EndpointOf)]
#[require(ConnectionStatus)]
pub struct ConnectionOf(pub Entity);

/// Inserted when a [`ConnectionOf`] component is inserted.
///
/// This component is immutable, so it's lifecycle events can be used to respond to changes in the connection status.
#[derive(Component, Default, Debug, Clone, Copy)]
#[component(immutable)]
pub enum ConnectionStatus {
    #[default]
    Connecting,
    Established,
    Closed,
    Failed,
}

/// An endpoint. Uses dynamic dispatch to provide a type erased api to a [`Transport`] implementation.
///
/// Use the [`Entity`] of the associated connection entities related by [`ConnectionOf`]
/// to obtain a [`Connection`] which can be used to perform stream operations.
#[derive(Component, Deref, DerefMut)]
pub struct Endpoint(Box<dyn Transport>);

impl Endpoint {
    pub fn new<T: Transport + 'static>(transport: T) -> Self {
        Endpoint(Box::new(transport))
    }

    pub fn as_transport<T: 'static>(&mut self) -> Option<&mut T> {
        self.as_any().downcast_mut()
    }
}

pub trait Transport: Send + Sync {
    fn as_any<'a>(&'a mut self) -> &'a mut dyn Any;

    /// Gets the [`Connection`] of an [`Entity`] that is related to this [`Endpoint`] by a [`ConnectionOf`].
    fn get_connection<'a>(
        &'a mut self,
        connection_entity: Entity,
    ) -> Result<Connection<'a>, NoConnectionError>;
}

#[derive(Deref, DerefMut)]
pub struct Connection<'a>(Box<dyn ConnectionContext + 'a>);

pub trait ConnectionContext {
    fn reborrow<'b>(&'b mut self) -> Connection<'b>;

    /// Returns a [`Stream`] id of a stream that meets certain [`StreamRequirements`].
    ///
    /// If ordering is not required then the implementation may return a stream id of an existing stream.
    fn new_stream(&mut self, requirements: StreamRequirements) -> Result<Stream>;

    /// Attempts to write a piece of data to a stream.
    ///
    /// If successful, the number of bytes written is returned.
    ///
    /// `block` determines what happens when the stream is congested.
    /// If `block` is set to false then bytes may be accepted and `Ok(1..)` values will be returned,
    /// however previously written data may be lost.
    /// If `block` is set to true then data will never be accepted if it cannot be sent.
    ///
    /// What does it mean for `block` to be `false` on a reliable stream?
    /// Some implementations may discard previously written chunks,
    /// but guarantee transmission of the most recently written one.
    /// For full reliability in all cases, always set `block` to `true`.
    fn write(&mut self, stream: &Stream, data: Bytes, block: bool) -> Result<usize>;

    /// Attempts to read a chunk of data from a stream.
    ///
    /// The return type has two nested results.
    /// If the first result is `Err` then there was some critical failure.
    /// The second result returns an `Err` when either the stream is blocked or closed.
    fn read(&mut self, stream: &Stream) -> Result<Result<Bytes, StreamReadError>>;

    /// Attempts to close a stream from the sending end.
    ///
    /// Not all streams require closing, but some do.
    /// All users should close streams when they are finished with them regardless.
    ///
    /// Some implementations may be able to either gracefully close, or reset streams.
    /// Setting `graceful` to `true` will guarantee that all written data will be transmitted.
    /// Ungracefully closing streams should be used when your own internal errors are encountered and transmitting remaining data is not important.
    fn close_send_stream(&mut self, stream: &Stream, graceful: bool) -> Result;

    /// Attempts to close a stream from the receiving end.
    ///
    /// Closing streams from the receiving end is never required and not always possible.
    /// Closing a stream is normally the sender's responsibility, but if you encounter an internal error
    /// and it is no longer important to receive remaining data, you can close the stream.
    ///
    /// It is still your responsibility to read any data that is produced by the stream.
    fn close_recv_stream(&mut self, stream: &Stream) -> Result;

    /// Returns a receive stream if there is a new one that has been opened by the client.
    /// Also returns the requirements that the stream is guaranteed to meet.
    /// If it is bidirectional this means that it is also a send stream.
    fn accept_stream(&mut self) -> Option<(Stream, StreamRequirements)>;

    /// Closes the connection.
    ///
    /// Calling multiple times does nothing.
    fn close(&mut self);

    /// Returns `true` when all reliable data on the connection has been sent.
    fn all_data_sent(&mut self) -> bool;
}

/// An id for a stream on a connection that is either send, receive or both.
///
/// Obtained by performing stream operations on a [`Connection`].
///
/// Internally it uses dynamic dispatch to type erase the specific implementation.
pub struct Stream(Box<dyn StreamId>);

impl Stream {
    pub fn as_stream<T: 'static>(&self) -> Result<&T> {
        Ok(self
            .0
            .as_any()
            .downcast_ref()
            .ok_or(MismatchedStreamError)?)
    }

    pub fn new<T: StreamId + 'static>(stream: T) -> Self {
        Self(Box::new(stream))
    }
}

pub trait StreamId: Send + Sync + 'static {
    fn as_any(&self) -> &dyn Any;

    /// Use to impl [`Clone`].
    fn clone(&self) -> Stream;

    /// Used to impl [`PartialEq`] and [`Eq`].
    fn eq(&self, rhs: &Stream) -> bool;
}

impl Clone for Stream {
    fn clone(&self) -> Self {
        self.0.clone()
    }
}

impl PartialEq for Stream {
    fn eq(&self, rhs: &Stream) -> bool {
        self.0.eq(rhs)
    }
}

/// Specifies the requirements for a stream when opening it.
///
/// When you create a stream you are garunteed to have *at least* these requirements if the operation is a success.
#[derive(Clone, Copy, Debug)]
pub struct StreamRequirements {
    pub reliable: bool,
    pub ordered: bool,
    pub bidirectional: bool,
}

impl StreamRequirements {
    /// No reliability, ordering, not bidirectional
    pub const UNRELIABLE: Self = Self {
        reliable: false,
        ordered: false,
        bidirectional: false,
    };

    /// Reliable, no ordering, not bidirectional
    pub const RELIABLE: Self = Self {
        reliable: true,
        ordered: false,
        bidirectional: false,
    };

    /// Reliable, ordered, not bidirectional
    pub const RELIABLE_ORDERED: Self = Self {
        reliable: true,
        ordered: true,
        bidirectional: false,
    };

    pub fn with_reliable(self, reliable: bool) -> Self {
        StreamRequirements { reliable, ..self }
    }

    pub fn with_ordered(self, ordered: bool) -> Self {
        StreamRequirements { ordered, ..self }
    }

    pub fn with_bidirectional(self, bidirectional: bool) -> Self {
        StreamRequirements {
            bidirectional,
            ..self
        }
    }
}

/// Reason why reading a stream wasn't successful, but not a critical failure.
pub enum StreamReadError {
    /// The stream is blocked, but may return more data later.
    Blocked,
    /// The stream is closed and will never yield more data.
    Closed,
}

#[derive(Error, Debug)]
#[error("This endpoint doesn't have a connection for this entity")]
pub struct NoConnectionError;

#[derive(Error, Debug)]
#[error("Stream was for a different type of transport")]
pub struct MismatchedStreamError;

#[derive(Error, Debug)]
#[error("Connection was unable to provide a stream with the requested requirements {0:?}")]
pub struct UnsupportedStreamRequirementsError(StreamRequirements);

#[derive(Error, Debug)]
#[error("The stream did not exist in a certain direction")]
pub struct NonexistentStreamError;
