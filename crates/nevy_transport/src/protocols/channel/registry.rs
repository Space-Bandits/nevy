//! Global registry for channel endpoints and pending connections.

use bevy::{platform::collections::HashMap, prelude::*};
use bytes::Bytes;
use crossbeam_channel::{Receiver, Sender, unbounded};

use crate::StreamRequirements;

/// Message type for channel-based communication.
#[derive(Debug, Clone)]
pub enum ChannelMessage {
    /// Open a new stream with the given requirements.
    StreamOpen {
        stream_id: u32,
        requirements: StreamRequirements,
    },
    /// Data for a stream.
    StreamData { stream_id: u32, data: Bytes },
    /// Close a stream.
    StreamClose { stream_id: u32 },
    /// Datagram message (unreliable, unordered).
    Datagram(Bytes),
}

/// A pending connection request from one endpoint to another.
pub struct ConnectionRequest {
    /// The endpoint entity that initiated the connection.
    pub from_endpoint: Entity,
    /// The connection entity on the initiating side.
    pub from_connection: Entity,
    /// Channel to send messages to the remote.
    pub to_remote_tx: Sender<ChannelMessage>,
    /// Channel to receive messages from the remote.
    pub from_remote_rx: Receiver<ChannelMessage>,
}

/// Handle for an endpoint in the registry.
pub(crate) struct ChannelEndpointHandle {
    /// Channel for receiving incoming connection requests.
    pub incoming_rx: Receiver<ConnectionRequest>,
    /// Channel for sending connection requests (kept for creating new connections).
    pub incoming_tx: Sender<ConnectionRequest>,
}

/// Global registry for channel endpoints.
///
/// This resource tracks all channel endpoints and facilitates connection establishment.
#[derive(Resource, Default)]
pub struct ChannelRegistry {
    /// Map from endpoint entity to its handle.
    pub(crate) endpoints: HashMap<Entity, ChannelEndpointHandle>,
}

impl ChannelRegistry {
    /// Register a new endpoint in the registry.
    pub fn register(&mut self, endpoint_entity: Entity) {
        let (tx, rx) = unbounded();
        self.endpoints.insert(
            endpoint_entity,
            ChannelEndpointHandle {
                incoming_rx: rx,
                incoming_tx: tx,
            },
        );
    }

    /// Unregister an endpoint from the registry.
    pub fn unregister(&mut self, endpoint_entity: Entity) {
        self.endpoints.remove(&endpoint_entity);
    }

    /// Get the sender for sending connection requests to an endpoint.
    pub fn get_incoming_tx(&self, endpoint_entity: Entity) -> Option<Sender<ConnectionRequest>> {
        self.endpoints
            .get(&endpoint_entity)
            .map(|h| h.incoming_tx.clone())
    }

    /// Try to receive a pending connection request for an endpoint.
    pub fn try_recv_incoming(
        &self,
        endpoint_entity: Entity,
    ) -> Option<ConnectionRequest> {
        self.endpoints
            .get(&endpoint_entity)
            .and_then(|h| h.incoming_rx.try_recv().ok())
    }
}
