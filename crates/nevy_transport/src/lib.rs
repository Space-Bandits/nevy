use std::any::Any;

use bevy::prelude::*;
use thiserror::Error;

pub mod prelude;
pub mod protocols;

pub const DEFAULT_TRANSPORT_SCHEDULE: PostUpdate = PostUpdate;

#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone, Copy)]
pub struct TransportUpdateSystems;

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

/// The lifecycle of this component is the lifecycle of its connection.
///
/// Inserting this component yourself is how you open a connection.
/// Some endpoints will provide entities with incoming connections,
/// and will accept them if you insert this component on that entity.
#[derive(Component, Deref)]
#[component(immutable)]
#[relationship(relationship_target = EndpointOf)]
#[require(ConnectionStatus)]
pub struct ConnectionOf(pub Entity);

#[derive(Component, Default, Debug, Clone, Copy)]
#[component(immutable)]
pub enum ConnectionStatus {
    #[default]
    Connecting,
    Established,
    Closed,
    Failed,
}

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

    fn get_connection<'a>(&'a mut self, connection_entity: Entity) -> Option<Connection<'a>>;
}

#[derive(Deref, DerefMut)]
pub struct Connection<'a>(Box<dyn ConnectionContext + 'a>);

pub trait ConnectionContext {
    fn reborrow<'b>(&'b mut self) -> Connection<'b>;

    fn new_stream(&mut self, requirements: StreamRequirements) -> Result<Stream, NewStreamError>;

    fn is_send_stream(&self, stream: &Stream) -> Result<bool, InvalidStreamError>;

    fn is_recv_stream(&self, stream: &Stream) -> Result<bool, InvalidStreamError>;

    fn write(&mut self, stream: &Stream, data: &[u8]) -> Result<usize, InvalidStreamError>;

    fn close_stream(&mut self, stream: &Stream, graceful: bool) -> Result<(), InvalidStreamError>;
}

pub struct Stream(Box<dyn StreamId>);

impl Stream {
    pub fn as_stream<T: 'static>(&self) -> Result<&T, MismatchedStreamError> {
        self.0.as_any().downcast_ref().ok_or(MismatchedStreamError)
    }

    pub fn new<T: StreamId + 'static>(stream: T) -> Self {
        Self(Box::new(stream))
    }
}

pub trait StreamId {
    fn as_any(&self) -> &dyn Any;

    fn clone(&self) -> Stream;
}

impl Clone for Stream {
    fn clone(&self) -> Self {
        self.0.clone()
    }
}

#[derive(Error, Debug)]
#[error("Stream was for a different type of transport")]
pub struct MismatchedStreamError;

/// Specifies the requirements for a stream when opening it.
///
/// When you create a stream you are garunteed to have *at least* these requirements if the operation is a success.
#[derive(Clone, Copy, Debug)]
pub struct StreamRequirements {
    ordered: bool,
    reliable: bool,
    bidirectional: bool,
}

#[derive(Error, Debug)]
#[error("Connection was unable to provide a stream with the requested requirements {0:?}")]
pub struct UnsupportedStreamRequirementsError(StreamRequirements);

#[derive(Error, Debug)]
#[error("Unable to create a new stream")]
pub enum NewStreamError {
    UnsupportedStreamRequirements(#[from] UnsupportedStreamRequirementsError),
    #[error("The transport layer was unable to create a new stream")]
    TransportError,
}

#[derive(Error, Debug)]
#[error("The stream did not exist in a certain direction")]
pub struct NonexistentStreamError;

#[derive(Error, Debug)]
#[error("The stream was invalid for this connection")]
pub enum InvalidStreamError {
    MismatchedStreamId(#[from] MismatchedStreamError),
    NonexistentStream(#[from] NonexistentStreamError),
}
