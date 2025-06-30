use std::{
    collections::VecDeque,
    io::IoSliceMut,
    net::{SocketAddr, ToSocketAddrs, UdpSocket},
    sync::Arc,
    time::Instant,
};

use bevy::{platform::collections::HashMap, prelude::*};

pub use quinn_proto;
pub use quinn_proto::ClientConfig;
use quinn_proto::{ConnectionHandle, DatagramEvent, Incoming};
use quinn_udp::{UdpSockRef, UdpSocketState};
use thiserror::Error;

use crate::{
    connection::ConnectionState, ConnectionOf, EndpointOf, NewConnectionOf, RemovedConnectionOf,
};

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
/// will close the connection with a code of `0` and an empty reason.
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
    trigger: Trigger<NewConnectionOf>,
    mut commands: Commands,
    mut endpoint_q: Query<&mut QuicEndpoint>,
    connection_q: Query<(Option<&QuicConnectionConfig>, Has<QuicConnection>)>,
) -> Result {
    let endpoint_entity = trigger.target();
    let connection_entity = trigger.event().0;

    // confirm that the endpoint has the right components
    let mut endpoint = endpoint_q.get_mut(endpoint_entity)?;

    let (config, opened_by_endpoint) = connection_q.get(connection_entity)?;

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
    trigger: Trigger<RemovedConnectionOf>,
    mut commands: Commands,
    mut endpoint_q: Query<&mut QuicEndpoint>,
    mut connection_q: Query<&QuicConnection>,
) -> Result {
    let endpoint_entity = trigger.target();
    let connection_entity = trigger.event().0;

    // remove the quic connection component
    commands
        .entity(connection_entity)
        .try_remove::<QuicConnection>()
        .try_remove::<ConnectionStatus>();

    // attempt to
    let Ok(mut endpoint) = endpoint_q.get_mut(endpoint_entity) else {
        return Ok(());
    };

    let Ok(connection) = connection_q.get_mut(connection_entity) else {
        return Ok(());
    };

    let Ok(connection) = endpoint.get_connection(connection) else {
        // there may not be connection state.
        return Ok(());
    };

    connection.close(0, default())?;

    // TODO: mark that the data will never be read and the connection state can be freed.

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
    pub fn new(
        bind_addr: impl ToSocketAddrs,
        endpoint_config: Option<quinn_proto::EndpointConfig>,
        server_config: Option<quinn_proto::ServerConfig>,
        incoming_handler: Box<dyn IncomingConnectionHandler>,
    ) -> Result<Self, std::io::Error> {
        let socket = UdpSocket::bind(bind_addr)?;
        let socket_state = UdpSocketState::new(UdpSockRef::from(&socket))?;
        let local_addr = socket.local_addr()?;

        let endpoint_config = endpoint_config.unwrap_or_default();
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

    pub fn set_incoming_handler(&mut self, incoming_handler: Box<dyn IncomingConnectionHandler>) {
        self.incoming_handler = incoming_handler.into();
    }

    /// Gets the connection state for a [QuicConnection]
    ///
    /// A state will always exist for a [Connecting](ConnectionStatus::Connecting) and [Established](ConnectionStatus::Established) connection,
    /// but might not for a [Closed](ConnectionStatus::Closed) connection.
    pub fn get_connection(
        &mut self,
        connection: &QuicConnection,
    ) -> Result<&mut ConnectionState, NoConnectionState> {
        self.connections
            .get_mut(&connection.connection_handle)
            // .map(|connection| ConnectionMut {
            //     connection,
            //     endpoint: &mut self.endpoint,
            // })
            .ok_or(NoConnectionState)
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
                    error!("An endpoint returned a connection event for a connection that doesn't exist");

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
        let max_gso_datagrams = self.socket_state.gro_segments();

        self.connections.retain(|&connection_handle, connection| {
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

            // Transmit any packets required
            while let Some(transmit) = {
                self.send_buffer.clear();

                connection.connection.poll_transmit(
                    std::time::Instant::now(),
                    max_gso_datagrams,
                    &mut self.send_buffer,
                )
            } {
                // the transmit failing is equivelant to dropping due to congestion, ignore error
                let _ = self.socket_state.send(
                    quinn_udp::UdpSockRef::from(&self.socket),
                    &udp_transmit(&transmit, &self.send_buffer),
                );
            }

            let now = std::time::Instant::now();
            while let Some(deadline) = connection.connection.poll_timeout() {
                if deadline <= now {
                    connection.connection.handle_timeout(now);
                } else {
                    break;
                }
            }

            let mut drop_connection_state = false;

            while let Some(endpoint_event) = connection.connection.poll_endpoint_events() {
                if endpoint_event.is_drained() {
                    drop_connection_state = true;
                }

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

            !drop_connection_state
        });
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

fn udp_transmit<'a>(
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
