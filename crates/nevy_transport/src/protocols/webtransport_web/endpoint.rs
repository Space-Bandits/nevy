//! Browser WebTransport endpoint implementation.

use std::{any::Any, collections::HashMap};

use bevy::prelude::*;
use js_sys::{Array, Object, Reflect, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{WebTransport, WebTransportOptions};

use crate::{Connection, ConnectionOf, ConnectionStatus, Endpoint, NoConnectionError, Transport};

use super::{
    connection::{ConnectedState, WebTransportWebConnectionContext},
    promise::PendingPromise,
};

/// Configuration for a browser WebTransport connection.
#[derive(Component, Clone)]
pub struct WebTransportWebConfig {
    /// The WebTransport server URL (e.g., "https://example.com:4433/game").
    pub url: String,
    /// Optional server certificate hashes for self-signed certificates.
    /// Each hash should be a SHA-256 hash of the certificate.
    pub server_certificate_hashes: Option<Vec<Vec<u8>>>,
}

/// Internal connection state.
pub(crate) enum WebConnection {
    /// Connection is being established.
    Connecting {
        transport: WebTransport,
        ready_promise: PendingPromise<()>,
    },
    /// Connection is established and ready for use.
    Connected(ConnectedState),
    /// Connection failed to establish.
    Failed(String),
    /// Connection has been closed.
    Closed,
}

struct EndpointUpdateContext<'w, 's> {
    commands: Commands<'w, 's>,
}

pub(super) fn update_endpoints(
    mut commands: Commands,
    mut endpoint_q: Query<(Entity, &mut Endpoint)>,
) {
    for (_endpoint_entity, mut endpoint) in endpoint_q.iter_mut() {
        let Some(endpoint) = endpoint.as_transport::<WebTransportWebEndpoint>() else {
            continue;
        };

        endpoint.update(EndpointUpdateContext {
            commands: commands.reborrow(),
        });
    }
}

pub(super) fn create_connections(
    insert: On<Insert, ConnectionOf>,
    mut commands: Commands,
    mut endpoint_q: Query<&mut Endpoint>,
    connection_q: Query<(&ConnectionOf, Option<&WebTransportWebConfig>)>,
) -> Result {
    let connection_entity = insert.entity;

    let (connection_of, config) = connection_q.get(connection_entity)?;

    let mut endpoint = endpoint_q.get_mut(**connection_of)?;

    let Some(endpoint) = endpoint.as_transport::<WebTransportWebEndpoint>() else {
        return Ok(());
    };

    let config = config.ok_or_else(|| {
        commands.entity(connection_entity).remove::<ConnectionOf>();
        format!(
            "No WebTransportWebConfig provided for connection {}",
            connection_entity
        )
    })?;

    // Create WebTransport options
    let options = WebTransportOptions::new();

    // Add server certificate hashes if provided (for self-signed certs)
    if let Some(ref hashes) = config.server_certificate_hashes {
        let hash_array = Array::new();

        for hash in hashes {
            let hash_obj = Object::new();
            Reflect::set(&hash_obj, &"algorithm".into(), &"sha-256".into()).ok();

            let hash_bytes = Uint8Array::from(hash.as_slice());
            Reflect::set(&hash_obj, &"value".into(), &hash_bytes).ok();

            hash_array.push(&hash_obj);
        }

        Reflect::set(&options, &"serverCertificateHashes".into(), &hash_array).ok();
    }

    // Create the WebTransport connection
    let transport = match WebTransport::new_with_options(&config.url, &options) {
        Ok(t) => t,
        Err(e) => {
            commands
                .entity(connection_entity)
                .insert(ConnectionStatus::Failed);

            endpoint.connections.insert(
                connection_entity,
                WebConnection::Failed(format!("{:?}", e)),
            );

            return Ok(());
        }
    };

    // Create promise for ready state
    let ready_promise = transport.ready();
    let pending = PendingPromise::new(ready_promise, |_| ());

    endpoint.connections.insert(
        connection_entity,
        WebConnection::Connecting {
            transport,
            ready_promise: pending,
        },
    );

    Ok(())
}

pub(super) fn remove_connections(
    replace: On<Replace, ConnectionOf>,
    connection_q: Query<&ConnectionOf>,
    mut endpoint_q: Query<&mut Endpoint>,
) -> Result {
    let connection_entity = replace.entity;

    let connection_of = connection_q.get(connection_entity)?;

    let Ok(mut endpoint) = endpoint_q.get_mut(**connection_of) else {
        return Ok(());
    };

    let Some(endpoint) = endpoint.as_transport::<WebTransportWebEndpoint>() else {
        return Ok(());
    };

    if let Some(conn) = endpoint.connections.remove(&connection_entity) {
        // Close the transport if connected
        match conn {
            WebConnection::Connecting { transport, .. } => {
                transport.close();
            }
            WebConnection::Connected(state) => {
                state.transport.close();
            }
            _ => {}
        }
    }

    Ok(())
}

/// Browser WebTransport endpoint implementation.
///
/// This endpoint uses the browser's native WebTransport API via web_sys.
/// It only supports client connections (browsers cannot act as WebTransport servers).
pub struct WebTransportWebEndpoint {
    pub(crate) connections: HashMap<Entity, WebConnection>,
}

// SAFETY: WASM is single-threaded, so Send/Sync is safe.
// These impls are required because Transport trait requires Send + Sync.
unsafe impl Send for WebTransportWebEndpoint {}
unsafe impl Sync for WebTransportWebEndpoint {}

impl WebTransportWebEndpoint {
    /// Create a new browser WebTransport endpoint.
    pub fn new() -> Endpoint {
        Endpoint::new(Self {
            connections: HashMap::new(),
        })
    }

    fn update(&mut self, mut context: EndpointUpdateContext) {
        let entities: Vec<_> = self.connections.keys().copied().collect();

        for entity in entities {
            let Some(conn) = self.connections.get_mut(&entity) else {
                continue;
            };

            match conn {
                WebConnection::Connecting {
                    transport,
                    ready_promise,
                } => {
                    if let Some(result) = ready_promise.poll() {
                        match result {
                            Ok(()) => {
                                // Connection established
                                let transport = transport.clone();
                                *conn = WebConnection::Connected(ConnectedState::new(transport));

                                context
                                    .commands
                                    .entity(entity)
                                    .insert(ConnectionStatus::Established);
                            }
                            Err(e) => {
                                *conn = WebConnection::Failed(format!("{:?}", e));

                                context
                                    .commands
                                    .entity(entity)
                                    .insert(ConnectionStatus::Failed);
                            }
                        }
                    }
                }
                WebConnection::Connected(state) => {
                    // Poll for incoming streams and datagrams
                    state.poll_incoming();
                }
                _ => {}
            }
        }
    }
}

impl Default for WebTransportWebEndpoint {
    fn default() -> Self {
        Self {
            connections: HashMap::new(),
        }
    }
}

impl Transport for WebTransportWebEndpoint {
    fn as_any<'a>(&'a mut self) -> &'a mut dyn Any {
        self
    }

    fn get_connection<'a>(
        &'a mut self,
        connection: Entity,
    ) -> Result<Connection<'a>, NoConnectionError> {
        let conn = self.connections.get_mut(&connection).ok_or(NoConnectionError)?;

        match conn {
            WebConnection::Connected(state) => Ok(Connection(Box::new(
                WebTransportWebConnectionContext::new(state),
            ))),
            _ => Err(NoConnectionError),
        }
    }
}
