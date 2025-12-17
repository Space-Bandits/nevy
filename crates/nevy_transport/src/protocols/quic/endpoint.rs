use std::{
    any::Any,
    collections::VecDeque,
    io::IoSliceMut,
    net::{SocketAddr, ToSocketAddrs, UdpSocket},
    sync::Arc,
    time::Instant,
};

use bevy::{platform::collections::HashMap, prelude::*};
use bytes::Bytes;
use log::{error, warn};
use quinn_proto::{
    ClientConfig, ConnectError, ConnectionError, ConnectionHandle, DatagramEvent, Incoming, VarInt,
};
use quinn_udp::{UdpSockRef, UdpSocketState};

use crate::{
    Connection, ConnectionOf, ConnectionStatus, Endpoint, Transport,
    protocols::quic::{
        connection::{CloseFlagState, QuicConnectionContext, QuicConnectionState},
        udp_transmit,
    },
};

#[derive(Component, Clone)]
pub struct QuicConnectionConfig {
    pub client_config: ClientConfig,
    pub address: SocketAddr,
    pub server_name: String,
}

#[derive(Component, Deref)]
pub struct QuicConnectionClosedReason(ConnectionError);

#[derive(Component, Deref)]
pub struct QuicConnectionFailedReason(ConnectError);

/// Holds the incoming connection. The incoming connection will be removed once it is used to accept or reject the connection.
#[derive(Component)]
pub struct IncomingQuicConnection {
    pub endpoint_entity: Entity,
    pub incoming: Option<Incoming>,
}

struct EndpointUpdateContext<'w, 's> {
    endpoint_entity: Entity,
    commands: Commands<'w, 's>,
}

pub(super) fn update_endpoints(
    mut commands: Commands,
    mut endpoint_q: Query<(Entity, &mut Endpoint)>,
) {
    for (endpoint_entity, mut endpoint) in endpoint_q.iter_mut() {
        let Some(endpoint) = endpoint.as_transport::<QuicEndpoint>() else {
            continue;
        };

        endpoint.update(EndpointUpdateContext {
            endpoint_entity,
            commands: commands.reborrow(),
        });
    }
}

pub(super) fn create_connections(
    insert: On<Insert, ConnectionOf>,
    mut commands: Commands,
    mut endpoint_q: Query<&mut Endpoint>,
    mut connection_q: Query<(
        &ConnectionOf,
        Option<&QuicConnectionConfig>,
        Option<&mut IncomingQuicConnection>,
    )>,
) -> Result {
    let connection_entity = insert.entity;

    let (connection_of, config, incoming) = connection_q.get_mut(connection_entity)?;

    // confirm that the endpoint has the right components
    let mut endpoint = endpoint_q.get_mut(**connection_of)?;

    let Some(endpoint) = endpoint.as_transport::<QuicEndpoint>() else {
        // Another transport layer is responsible for this connection.
        return Ok(());
    };

    if let Some(mut incoming) = incoming {
        commands
            .entity(connection_entity)
            .remove::<IncomingQuicConnection>();

        let Some(incoming) = incoming.incoming.take() else {
            error!("User tried to accept an incoming connection twice.");
            return Ok(());
        };

        match endpoint.endpoint.accept(
            incoming,
            std::time::Instant::now(),
            &mut endpoint.send_buffer,
            None,
        ) {
            Err(err) => {
                if let Some(transmit) = err.response {
                    let _ = endpoint.socket_state.send(
                        UdpSockRef::from(&endpoint.socket),
                        &udp_transmit(&transmit, &endpoint.send_buffer),
                    );
                }
            }
            Ok((connection_handle, connection)) => {
                endpoint
                    .connection_handles
                    .insert(connection_entity, connection_handle);

                endpoint.connections.insert(
                    connection_handle,
                    QuicConnectionState {
                        connection_entity,
                        connection,
                        stream_events: VecDeque::new(),
                        close: CloseFlagState::None,
                        datagrams_queue_offset: 0,
                        datagrams: VecDeque::new(),
                    },
                );
            }
        };
    } else {
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
                    .insert((ConnectionStatus::Failed, QuicConnectionFailedReason(error)));

                return Ok(());
            }
        };

        endpoint
            .connection_handles
            .insert(connection_entity, connection_handle);

        endpoint.connections.insert(
            connection_handle,
            QuicConnectionState {
                connection_entity,
                connection,
                stream_events: VecDeque::new(),
                close: CloseFlagState::None,
                datagrams_queue_offset: 0,
                datagrams: VecDeque::new(),
            },
        );
    }

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

    let Some(endpoint) = endpoint.as_transport::<QuicEndpoint>() else {
        return Ok(());
    };

    // ungracefully drop the connection state

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

pub(super) fn refuse_connections(
    replace: On<Replace, IncomingQuicConnection>,
    mut endpoint_q: Query<&mut Endpoint>,
    mut connection_q: Query<&mut IncomingQuicConnection>,
) -> Result {
    let connection_entity = replace.entity;

    let mut incoming_component = connection_q.get_mut(connection_entity)?;

    if let Some(incoming) = incoming_component.incoming.take() {
        // if the incoming connection still exists when the component is removed, refuse the connection.

        let mut endpoint = endpoint_q.get_mut(incoming_component.endpoint_entity)?;

        let endpoint = endpoint
            .as_transport::<QuicEndpoint>()
            .ok_or("An incoming quic connection should always be pointing to a quic endpoint")?;

        let transmit = endpoint
            .endpoint
            .refuse(incoming, &mut endpoint.send_buffer);

        let _ = endpoint.socket_state.send(
            UdpSockRef::from(&endpoint.socket),
            &udp_transmit(&transmit, &endpoint.send_buffer),
        );
    }

    Ok(())
}

pub struct QuicEndpoint {
    endpoint: quinn_proto::Endpoint,
    connection_handles: HashMap<Entity, ConnectionHandle>,
    connections: HashMap<ConnectionHandle, QuicConnectionState>,
    socket: UdpSocket,
    socket_state: quinn_udp::UdpSocketState,
    local_addr: SocketAddr,
    endpoint_config: quinn_proto::EndpointConfig,
    server_config: Option<quinn_proto::ServerConfig>,
    recv_buffer: Vec<u8>,
    send_buffer: Vec<u8>,
}

impl QuicEndpoint {
    /// The method to construct an endpoint.
    ///
    /// This constructor takes
    /// - A bind address for the UDP socket.
    /// - A [quin_proto] endpoint config
    /// - A [quin_proto] server config. If this is `None` then the endpoint won't be able to accept incoming connections.
    pub fn new(
        bind_addr: impl ToSocketAddrs,
        endpoint_config: quinn_proto::EndpointConfig,
        server_config: Option<quinn_proto::ServerConfig>,
    ) -> Result<Endpoint, std::io::Error> {
        let socket = UdpSocket::bind(bind_addr)?;
        let socket_state = UdpSocketState::new(UdpSockRef::from(&socket))?;
        let local_addr = socket.local_addr()?;

        let endpoint = quinn_proto::Endpoint::new(
            Arc::new(endpoint_config.clone()),
            server_config.clone().map(Arc::new),
            true,
            None,
        );

        let endpoint = Self {
            endpoint,
            connection_handles: HashMap::new(),
            connections: HashMap::new(),
            socket,
            socket_state,
            local_addr,
            endpoint_config,
            server_config,
            recv_buffer: Vec::new(),
            send_buffer: Vec::new(),
        };

        Ok(Endpoint::new(endpoint))
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
            error!(
                "remote address {} attempted to connect on endpoint {} {} but the endpoint isn't configured as a server",
                incoming.remote_address(),
                context.endpoint_entity,
                self.local_addr
            );
            return Some(self.endpoint.refuse(incoming, &mut self.send_buffer));
        }

        context.commands.spawn(IncomingQuicConnection {
            endpoint_entity: context.endpoint_entity,
            incoming: Some(incoming),
        });

        None
    }

    fn update_connections(&mut self, context: &mut EndpointUpdateContext) {
        for (&connection_handle, connection) in self.connections.iter_mut() {
            if let CloseFlagState::Sent = connection.close {
                connection.close = CloseFlagState::Received;

                connection
                    .connection
                    .close(Instant::now(), VarInt::from_u32(0), Bytes::new());

                context
                    .commands
                    .entity(connection.connection_entity)
                    .insert(ConnectionStatus::Closed);
            }

            QuicConnectionContext {
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
                            .insert((ConnectionStatus::Closed, QuicConnectionClosedReason(reason)));
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

impl Transport for QuicEndpoint {
    fn as_any<'a>(&'a mut self) -> &'a mut dyn Any {
        self
    }

    fn get_connection<'a>(&'a mut self, connection: Entity) -> Option<Connection<'a>> {
        let handle = self.connection_handles.get(&connection)?;

        let connection = self.connections.get_mut(handle)?;

        Some(Connection(Box::new(QuicConnectionContext {
            connection,
            send_buffer: &mut self.send_buffer,
            max_datagrams: self.socket_state.gro_segments(),
            socket: &self.socket,
            socket_state: &self.socket_state,
        })))
    }
}
