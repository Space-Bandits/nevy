use std::{
    collections::VecDeque,
    io::IoSliceMut,
    net::{SocketAddr, ToSocketAddrs, UdpSocket},
    sync::Arc,
    time::Instant,
};

use bevy::{platform::collections::HashMap, prelude::*};

use log::{error, warn};
use quinn_proto::{ClientConfig, ConnectionHandle, DatagramEvent, Incoming};
use quinn_udp::{UdpSockRef, UdpSocketState};
use thiserror::Error;

use crate::{ConnectionMut, ConnectionOf, EndpointOf, connection::ConnectionState, udp_transmit};

/// Must be inserted onto a connection entity to open a connection.
///
/// This component is not removed or modified and can be reused if a connection is opened and closed multiple times.
/// It will not be inserted when a connection is accepted.
#[derive(Component, Clone)]
pub struct QuicConnectionConfig {
    pub client_config: ClientConfig,
    pub address: SocketAddr,
    pub server_name: String,
}

/// A quic endpoint.
///
/// This component contains the entire state machine for the endpoint and all connections.
/// Querying this component is required for sending and receiving data on any of the connections.
#[derive(Component)]
#[require(EndpointOf)]
pub struct QuicEndpoint {
    endpoint: quinn_proto::Endpoint,
    connections: HashMap<ConnectionHandle, ConnectionState>,
    socket: UdpSocket,
    socket_state: quinn_udp::UdpSocketState,
    local_addr: SocketAddr,
    endpoint_config: quinn_proto::EndpointConfig,
    server_config: Option<quinn_proto::ServerConfig>,
    recv_buffer: Vec<u8>,
    send_buffer: Vec<u8>,
    pub incoming_handler: Box<dyn IncomingConnectionHandler>,
}

/// Component that exists on an entity when it is a [ConnectionOf] a [QuicEndpoint].
///
/// Used to identify the quic connection state within it's associated endpoint.
///
/// This component existing does not mean the connection is established.
/// Use [ConnectionStatus] to respond to the connecting, established and closed lifecycle stages.
///
/// This component will not be removed when the connection is closed, this is your responsibility.
/// Removing this component by removing the [ConnectionOf](crate::ConnectionOf)
/// will drop the connection ungracefully.
#[derive(Component)]
#[component(immutable)]
#[require(ConnectionStatus)]
pub struct QuicConnection {
    /// Contains the connection handle that is mapped to this entity in the [QuicEndpoint] of this connection.
    connection_handle: ConnectionHandle,
}

/// The status of a [QuicConnection].
///
/// This component is immutable, it's lifecycle events can be used to respond to updates.
#[derive(Component, Default)]
#[component(immutable)]
pub enum ConnectionStatus {
    /// The initial state for a [QuicConnection].
    #[default]
    Connecting,
    /// The connection is established and ready to use.
    Established,
    /// The connection is closed.
    /// You may still be able to get the connection state from the [QuicEndpoint] if there is unread data,
    /// otherwise the [ConnectionOf] component can be removed without losing data.
    Closed {
        reason: quinn_proto::ConnectionError,
    },
    /// The connection enters this state when the [QuicEndpoint] couldn't open a connection.
    ///
    /// In this case no [QuicEndpoint] component will exist
    Failed { error: quinn_proto::ConnectError },
}

pub(crate) fn inserted_connection_of_observer(
    event: On<Insert, ConnectionOf>,
    mut commands: Commands,
    mut endpoint_q: Query<&mut QuicEndpoint>,
    connection_q: Query<(
        &ConnectionOf,
        Option<&QuicConnectionConfig>,
        Has<QuicConnection>,
    )>,
) -> Result {
    let connection_entity = event.entity;

    let (connection_of, config, opened_by_endpoint) = connection_q.get(connection_entity)?;

    // confirm that the endpoint has the right components
    let mut endpoint = endpoint_q.get_mut(**connection_of)?;

    if !opened_by_endpoint {
        // this connection was inserted by the application. Open a connection

        let connection_config = config.ok_or_else(|| {
            commands.entity(connection_entity).remove::<ConnectionOf>();

            format!(
                "No connection config provided for quic connection {}. Removing `ConnectionOf`",
                connection_entity
            )
        })?;

        let (connection_handle, connection) = match endpoint.endpoint.connect(
            Instant::now(),
            connection_config.client_config.clone(),
            connection_config.address,
            &connection_config.server_name,
        ) {
            Ok(connection) => connection,
            Err(error) => {
                warn!(
                    "Failed to open connection to {} {}",
                    connection_config.address, connection_config.server_name
                );

                commands
                    .entity(connection_entity)
                    .insert(ConnectionStatus::Failed { error });

                return Ok(());
            }
        };

        endpoint.connections.insert(
            connection_handle,
            ConnectionState {
                connection_entity,
                connection,
                stream_events: VecDeque::new(),
                close: None,
            },
        );

        commands
            .entity(connection_entity)
            .insert(QuicConnection { connection_handle });
    }

    Ok(())
}

pub(crate) fn removed_connection_of_observer(
    event: On<Replace, ConnectionOf>,
    mut commands: Commands,
    connection_q: Query<&ConnectionOf>,
    mut endpoint_q: Query<&mut QuicEndpoint>,
) -> Result {
    let connection_entity = event.entity;

    let connection_of = connection_q.get(connection_entity)?;

    // remove associated components
    commands
        .entity(connection_entity)
        .try_remove::<QuicConnection>();

    // ungracefully drop the connection state

    let Ok(mut endpoint) = endpoint_q.get_mut(**connection_of) else {
        return Ok(());
    };

    if let Some((connection_handle, already_drained)) =
        endpoint
            .connections
            .iter()
            .find_map(|(connection_handle, connection)| {
                if connection.connection_entity != connection_entity {
                    return None;
                }

                Some((*connection_handle, connection.connection.is_drained()))
            })
    {
        if !already_drained {
            endpoint
                .endpoint
                .handle_event(connection_handle, quinn_proto::EndpointEvent::drained());
        }

        endpoint.connections.remove(&connection_handle);
    }

    Ok(())
}

pub(crate) struct EndpointUpdateContext<'w, 's> {
    endpoint_entity: Entity,
    commands: Commands<'w, 's>,
}

pub(crate) fn update_endpoints(
    mut commands: Commands,
    mut endpoint_q: Query<(Entity, &mut QuicEndpoint)>,
) {
    for (endpoint_entity, mut endpoint) in endpoint_q.iter_mut() {
        endpoint.update(EndpointUpdateContext {
            endpoint_entity,
            commands: commands.reborrow(),
        });
    }
}

impl QuicEndpoint {
    /// The method to construct an endpoint.
    ///
    /// This constructor takes
    /// - A bind address for the UDP socket.
    /// - A [quin_proto] endpoint config
    /// - A [quin_proto] server config. If this is `None` then the endpoint won't be able to accept incoming connections.
    /// - An [IncomingConnectionHandler] responsible for deciding whether to accept connections.
    pub fn new(
        bind_addr: impl ToSocketAddrs,
        endpoint_config: quinn_proto::EndpointConfig,
        server_config: Option<quinn_proto::ServerConfig>,
        incoming_handler: Box<dyn IncomingConnectionHandler>,
    ) -> Result<Self, std::io::Error> {
        let socket = UdpSocket::bind(bind_addr)?;
        let socket_state = UdpSocketState::new(UdpSockRef::from(&socket))?;
        let local_addr = socket.local_addr()?;

        let endpoint = quinn_proto::Endpoint::new(
            Arc::new(endpoint_config.clone()),
            server_config.clone().map(Arc::new),
            true,
            None,
        );

        Ok(Self {
            endpoint,
            connections: HashMap::new(),
            socket,
            socket_state,
            local_addr,
            endpoint_config,
            server_config,
            recv_buffer: Vec::new(),
            send_buffer: Vec::new(),
            incoming_handler: incoming_handler,
        })
    }

    /// Replaces the current [IncomingConnectionHandler] and returns it.
    pub fn set_incoming_handler(
        &mut self,
        incoming_handler: Box<dyn IncomingConnectionHandler>,
    ) -> Box<dyn IncomingConnectionHandler> {
        std::mem::replace(&mut self.incoming_handler, incoming_handler)
    }

    /// Gets the connection state for a [QuicConnection] associated with this endpoint.
    pub fn get_connection<'a>(
        &'a mut self,
        connection: &QuicConnection,
    ) -> Result<ConnectionMut<'a>, NoConnectionState> {
        self.connections
            .get_mut(&connection.connection_handle)
            .ok_or(NoConnectionState)
            .map(|connection| ConnectionMut {
                connection,
                send_buffer: &mut self.send_buffer,
                max_datagrams: self.socket_state.gro_segments(),
                socket: &self.socket,
                socket_state: &self.socket_state,
            })
    }

    fn update(&mut self, mut context: EndpointUpdateContext) {
        self.receive_datagrams(&mut context);
        self.update_connections(&mut context);
    }

    /// Will receive all datagrams from the socket and process events produced.
    ///
    /// This includes
    fn receive_datagrams(&mut self, context: &mut EndpointUpdateContext) {
        let mut recv_buffer = std::mem::take(&mut self.recv_buffer);

        let min_buffer_len = self
            .endpoint_config
            .get_max_udp_payload_size()
            .min(64 * 1024) as usize
            * self.socket_state.max_gso_segments()
            * quinn_udp::BATCH_SIZE;

        recv_buffer.resize(min_buffer_len, 0);
        let buffer_len = recv_buffer.len();

        let mut buffer_chunks = recv_buffer
            .chunks_mut(buffer_len / quinn_udp::BATCH_SIZE)
            .map(IoSliceMut::new);

        // unwrap is safe here because we know we have at least one chunk based on established buffer len.
        let mut buffer_chunks: [IoSliceMut; quinn_udp::BATCH_SIZE] =
            std::array::from_fn(|_| buffer_chunks.next().unwrap());

        let mut metas = [quinn_udp::RecvMeta::default(); quinn_udp::BATCH_SIZE];
        loop {
            match self.socket_state.recv(
                UdpSockRef::from(&self.socket),
                &mut buffer_chunks,
                &mut metas,
            ) {
                Ok(datagram_count) => {
                    self.handle_packet(datagram_count, &buffer_chunks, &metas, context);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {}
                Err(other) => {
                    error!(
                        "Received an unexpected error while receiving endpoint datagrams: {:?}",
                        other
                    )
                }
            }
        }

        self.send_buffer.clear();
        recv_buffer.clear();
        self.recv_buffer = recv_buffer;
    }

    fn handle_packet(
        &mut self,
        datagram_count: usize,
        buffer_chunks: &[IoSliceMut; quinn_udp::BATCH_SIZE],
        metas: &[quinn_udp::RecvMeta; quinn_udp::BATCH_SIZE],
        context: &mut EndpointUpdateContext,
    ) {
        for (meta, buffer) in metas.iter().zip(buffer_chunks.iter()).take(datagram_count) {
            let mut remaining_data = &buffer[0..meta.len];
            while !remaining_data.is_empty() {
                let stride_length = meta.stride.min(remaining_data.len());
                let data = &remaining_data[0..stride_length];
                remaining_data = &remaining_data[stride_length..];

                let ecn = meta.ecn.map(|ecn| match ecn {
                    quinn_udp::EcnCodepoint::Ect0 => quinn_proto::EcnCodepoint::Ect0,
                    quinn_udp::EcnCodepoint::Ect1 => quinn_proto::EcnCodepoint::Ect1,
                    quinn_udp::EcnCodepoint::Ce => quinn_proto::EcnCodepoint::Ce,
                });

                let Some(datagram_event) = self.endpoint.handle(
                    std::time::Instant::now(),
                    meta.addr,
                    meta.dst_ip,
                    ecn,
                    data.into(),
                    &mut self.send_buffer,
                ) else {
                    continue;
                };

                self.handle_datagram_event(datagram_event, context);
            }
        }
    }

    /// Handles an event produced by receiving a datagram.
    fn handle_datagram_event(&mut self, event: DatagramEvent, context: &mut EndpointUpdateContext) {
        let transmit = match event {
            DatagramEvent::NewConnection(incoming) => {
                self.handle_incoming_connection(incoming, context)
            }
            DatagramEvent::ConnectionEvent(connection_handle, event) => {
                let Some(connection) = self.connections.get_mut(&connection_handle) else {
                    error!(
                        "An endpoint returned a connection event for a connection that doesn't exist"
                    );

                    return;
                };

                connection.connection.handle_event(event);

                None
            }
            DatagramEvent::Response(transmit) => Some(transmit),
        };

        if let Some(transmit) = transmit {
            // the transmit failing is equivelant to dropping due to congestion
            let _ = self.socket_state.send(
                quinn_udp::UdpSockRef::from(&self.socket),
                &udp_transmit(&transmit, &self.send_buffer),
            );
        }
    }

    /// Handles an incomong connection.
    fn handle_incoming_connection(
        &mut self,
        incoming: Incoming,
        context: &mut EndpointUpdateContext,
    ) -> Option<quinn_proto::Transmit> {
        if self.server_config.is_none() {
            warn!(
                "remote address {} attempted to connect on endpoint {} {} but the endpoint isn't configured as a server",
                incoming.remote_address(),
                context.endpoint_entity,
                self.local_addr
            );
            return Some(self.endpoint.refuse(incoming, &mut self.send_buffer));
        }

        // use the incoming connection handler to decide whether to accept the connection
        if !self.incoming_handler.request(&incoming) {
            return Some(self.endpoint.refuse(incoming, &mut self.send_buffer));
        };

        match self.endpoint.accept(
            incoming,
            std::time::Instant::now(),
            &mut self.send_buffer,
            None,
        ) {
            Err(err) => return err.response,
            Ok((connection_handle, connection)) => {
                let connection_entity = context
                    .commands
                    .spawn((
                        ConnectionOf(context.endpoint_entity),
                        QuicConnection { connection_handle },
                    ))
                    .id();

                self.connections.insert(
                    connection_handle,
                    ConnectionState {
                        connection_entity,
                        connection,
                        stream_events: VecDeque::new(),
                        close: None,
                    },
                );

                None
            }
        }
    }

    fn update_connections(&mut self, context: &mut EndpointUpdateContext) {
        for (&connection_handle, connection) in self.connections.iter_mut() {
            if let Some((code, reason)) = connection.close.take() {
                connection
                    .connection
                    .close(Instant::now(), code, reason.into());

                context
                    .commands
                    .entity(connection.connection_entity)
                    .insert(ConnectionStatus::Closed {
                        reason: quinn_proto::ConnectionError::LocallyClosed,
                    });
            }

            ConnectionMut {
                connection,
                send_buffer: &mut self.send_buffer,
                max_datagrams: self.socket_state.gro_segments(),
                socket: &self.socket,
                socket_state: &self.socket_state,
            }
            .transmit_packets();

            let now = std::time::Instant::now();
            while let Some(deadline) = connection.connection.poll_timeout() {
                if deadline <= now {
                    connection.connection.handle_timeout(now);
                } else {
                    break;
                }
            }

            while let Some(endpoint_event) = connection.connection.poll_endpoint_events() {
                if let Some(connection_event) = self
                    .endpoint
                    .handle_event(connection_handle, endpoint_event)
                {
                    connection.connection.handle_event(connection_event);
                }
            }

            while let Some(app_event) = connection.connection.poll() {
                match app_event {
                    quinn_proto::Event::HandshakeDataReady => {}
                    quinn_proto::Event::Connected => {
                        context
                            .commands
                            .entity(connection.connection_entity)
                            .insert(ConnectionStatus::Established);
                    }
                    quinn_proto::Event::ConnectionLost { reason } => {
                        context
                            .commands
                            .entity(connection.connection_entity)
                            .insert(ConnectionStatus::Closed { reason });
                    }
                    quinn_proto::Event::Stream(event) => {
                        connection.stream_events.push_back(event.into());
                    }
                    quinn_proto::Event::DatagramReceived => {}
                    quinn_proto::Event::DatagramsUnblocked => {}
                }
            }
        }
    }
}

/// Returned when attempting to get the connection state from a [QuicEndpoint] with an invalid [QuicConnection]
#[derive(Clone, Copy, Debug, Error)]
#[error("Connection state does not exist on this endpoint")]
pub struct NoConnectionState;

/// Implement this trait to define logic for handling incoming connections.
pub trait IncomingConnectionHandler: Send + Sync + 'static {
    fn request(&mut self, incoming: &Incoming) -> bool;
}
