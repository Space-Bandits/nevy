use std::{
    io::IoSliceMut,
    net::{SocketAddr, ToSocketAddrs, UdpSocket},
    sync::Arc,
    time::Instant,
};

use bevy::{
    ecs::{intern::Interned, schedule::ScheduleLabel},
    platform::collections::HashMap,
    prelude::*,
};
use nevy::*;

pub use quinn_proto;
pub use quinn_proto::ClientConfig;
use quinn_proto::{ConnectionEvent, ConnectionHandle, DatagramEvent, Incoming};
use quinn_udp::{UdpSockRef, UdpSocketState};

pub struct NevyQuic {
    schedule: Interned<dyn ScheduleLabel>,
}

impl NevyQuic {
    pub fn new(schedule: impl ScheduleLabel) -> Self {
        NevyQuic {
            schedule: schedule.intern(),
        }
    }
}

impl Default for NevyQuic {
    fn default() -> Self {
        Self::new(PostUpdate)
    }
}

impl Plugin for NevyQuic {
    fn build(&self, app: &mut App) {
        app.add_observer(new_connection_observer);
        app.add_observer(removed_connection_observer);

        app.add_systems(self.schedule, update_endpoints);
    }
}

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

/// Component that exists on an entity when it is a [ConnectionOf] a [QuicEndpoint].
#[derive(Component)]
#[require(ConnectionOf)]
pub struct QuicConnection {
    /// Contains the connection handle that is mapped to this entity in the [QuicEndpoint] of this connection.
    connection_handle: Option<ConnectionHandle>,
}

/// Contains a quinn state machine for a quic endpoint.
/// Insert onto an entity to create a quic endpoint.
#[derive(Component)]
#[require(EndpointOf)]
pub struct QuicEndpoint {
    endpoint: quinn_proto::Endpoint,
    connections: HashMap<ConnectionHandle, quinn_proto::Connection>,
    socket: UdpSocket,
    socket_state: quinn_udp::UdpSocketState,
    local_addr: SocketAddr,
    endpoint_config: quinn_proto::EndpointConfig,
    server_config: Option<quinn_proto::ServerConfig>,
    recv_buffer: Vec<u8>,
    send_buffer: Vec<u8>,
    pub incoming_handler: Box<dyn IncomingConnectionHandler>,
    /// Contains the associated entity for a connection handle if it has been initialized.
    ///
    /// Is immediately removed when the [ConnectionOf] component is removed.
    connection_entities: HashMap<ConnectionHandle, Entity>,
}

fn new_connection_observer(
    trigger: Trigger<NewConnectionOf>,
    mut commands: Commands,
    endpoint_q: Query<(), With<QuicEndpoint>>,
    connection_q: Query<(Option<&QuicConnectionConfig>, Option<&QuicConnection>)>,
) {
    let endpoint_entity = trigger.target();
    let connection_entity = trigger.event().0;

    if !endpoint_q.contains(endpoint_entity) {
        return;
    }

    let Ok((config, connection)) = connection_q.get(connection_entity) else {
        return;
    };

    if let Some(QuicConnection {
        connection_handle: Some(_),
        ..
    }) = connection
    {
        // connection accepted by endpoint

        debug!(
            "accepted new connection {} on endpoint {}",
            connection_entity, endpoint_entity
        );
    } else {
        // connection opened by application

        let Some(_config) = config else {
            error!(
                "No connection config provided for quic connection {}. Removing connection.",
                connection_entity
            );

            commands.entity(connection_entity).remove::<ConnectionOf>();

            return;
        };

        commands.entity(connection_entity).insert(QuicConnection {
            connection_handle: None,
        });

        debug!(
            "opened new connection {} on endpoint {}",
            connection_entity, endpoint_entity
        );
    }
}

fn removed_connection_observer(
    trigger: Trigger<RemovedConnectionOf>,
    mut commands: Commands,
    mut endpoint_q: Query<&mut QuicEndpoint>,
    mut connection_q: Query<&mut QuicConnection>,
) {
    let endpoint_entity = trigger.target();
    let connection_entity = trigger.event().0;

    let Ok(mut endpoint) = endpoint_q.get_mut(endpoint_entity) else {
        return;
    };

    let Ok(mut connection) = connection_q.get_mut(connection_entity) else {
        return;
    };

    if let Some(connection_handle) = connection.connection_handle.take() {
        endpoint.connections.remove(&connection_handle);
    }

    commands
        .entity(connection_entity)
        .remove::<QuicConnection>();
}

fn update_endpoints(
    mut commands: Commands,
    mut endpoint_q: Query<(Entity, &mut QuicEndpoint)>,
    mut new_connections: Query<
        (
            Entity,
            &ConnectionOf,
            &mut QuicConnection,
            &QuicConnectionConfig,
        ),
        Added<QuicConnection>,
    >,
) {
    for (connection_entity, connection_of, mut connection, connection_config) in
        new_connections.iter_mut()
    {
        let None = connection.connection_handle else {
            // connection was accepted not opened
            continue;
        };

        let Ok((_, mut endpoint)) = endpoint_q.get_mut(**connection_of) else {
            error!(
                "Couldn't query connection {}'s endpoint {}",
                connection_entity, **connection_of
            );

            continue;
        };

        let Ok((connection_handle, connection_state)) = endpoint.endpoint.connect(
            Instant::now(),
            connection_config.client_config.clone(),
            connection_config.address,
            &connection_config.server_name,
        ) else {
            error!(
                "Failed to open connection to {} {}",
                connection_config.address, connection_config.server_name
            );

            commands.entity(connection_entity).remove::<ConnectionOf>();

            continue;
        };

        endpoint
            .connections
            .insert(connection_handle, connection_state);

        endpoint
            .connection_entities
            .insert(connection_handle, connection_entity);

        connection.connection_handle = Some(connection_handle);
    }

    for (endpoint_entity, mut endpoint) in endpoint_q.iter_mut() {
        endpoint.receive_datagrams(endpoint_entity, &mut commands);
        endpoint.update_connections();
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
            connection_entities: HashMap::new(),
        })
    }

    pub fn set_incoming_handler(&mut self, incoming_handler: Box<dyn IncomingConnectionHandler>) {
        self.incoming_handler = incoming_handler.into();
    }

    fn receive_datagrams(&mut self, endpoint_entity: Entity, commands: &mut Commands) {
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
                    self.handle_packet(
                        datagram_count,
                        &buffer_chunks,
                        &metas,
                        endpoint_entity,
                        commands,
                    );
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {
                    info!("todo: close connection on connection reset")
                }
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
        endpoint_entity: Entity,
        commands: &mut Commands,
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

                self.handle_datagram_event(datagram_event, endpoint_entity, commands);
            }
        }
    }

    fn handle_datagram_event(
        &mut self,
        event: DatagramEvent,
        endpoint_entity: Entity,
        commands: &mut Commands,
    ) {
        let transmit = match event {
            DatagramEvent::NewConnection(incoming) => {
                self.handle_new_connection(incoming, endpoint_entity, commands)
            }
            DatagramEvent::ConnectionEvent(connection_handle, event) => {
                self.handle_connection_event(connection_handle, event);
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

    fn handle_new_connection(
        &mut self,
        incoming: Incoming,
        endpoint_entity: Entity,
        commands: &mut Commands,
    ) -> Option<quinn_proto::Transmit> {
        if self.server_config.is_none() {
            warn!(
                "{} attempted to connect to endpoint {} but the endpoint isn't configured as a server",
                incoming.remote_address(),
                self.local_addr
            );
            return Some(self.endpoint.refuse(incoming, &mut self.send_buffer));
        }

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
                self.connections.insert(connection_handle, connection);

                let connection_entity = commands
                    .spawn((
                        ConnectionOf(endpoint_entity),
                        QuicConnection {
                            connection_handle: Some(connection_handle),
                        },
                    ))
                    .id();

                self.connection_entities
                    .insert(connection_handle, connection_entity);

                None
            }
        }
    }

    fn handle_connection_event(
        &mut self,
        connection_handle: ConnectionHandle,
        event: ConnectionEvent,
    ) {
        let Some(connection) = self.connections.get_mut(&connection_handle) else {
            error!("An endpoint returned a connection event for a connection that doesn't exist");

            dbg!(self.connections.len());

            return;
        };

        connection.handle_event(event);
    }

    fn update_connections(&mut self) {
        let max_gso_datagrams = self.socket_state.gro_segments();

        self.connections.retain(|&connection_handle, connection| {
            // Return transmission to endpoint if there is one.
            self.send_buffer.clear();
            if let Some(transmit) = connection.poll_transmit(
                std::time::Instant::now(),
                max_gso_datagrams,
                &mut self.send_buffer,
            ) {
                // the transmit failing is equivelant to dropping due to congestion
                let _ = self.socket_state.send(
                    quinn_udp::UdpSockRef::from(&self.socket),
                    &udp_transmit(&transmit, &self.send_buffer),
                );
            }

            let now = std::time::Instant::now();
            while let Some(deadline) = connection.poll_timeout() {
                if deadline <= now {
                    connection.handle_timeout(now);
                } else {
                    break;
                }
            }

            let mut drop_connection_state = false;

            while let Some(endpoint_event) = connection.poll_endpoint_events() {
                if endpoint_event.is_drained() {
                    drop_connection_state = true;
                }

                if let Some(connection_event) = self
                    .endpoint
                    .handle_event(connection_handle, endpoint_event)
                {
                    connection.handle_event(connection_event);
                }
            }

            while let Some(app_event) = connection.poll() {
                match app_event {
                    quinn_proto::Event::HandshakeDataReady => (),
                    quinn_proto::Event::Connected => info!("todo: connection established"),
                    quinn_proto::Event::ConnectionLost { reason: _ } => {
                        info!("todo: connection lost")
                    }
                    quinn_proto::Event::Stream(_s) => {}
                    quinn_proto::Event::DatagramReceived => {}
                    quinn_proto::Event::DatagramsUnblocked => {}
                }
            }

            while let Some(stream_handle) = connection.streams().accept(quinn_proto::Dir::Uni) {
                info!("todo: new unidirectional stream");
            }

            while let Some(stream_handle) = connection.streams().accept(quinn_proto::Dir::Bi) {
                info!("todo: new bidirectional stream");
            }

            !drop_connection_state
        });
    }
}

pub trait IncomingConnectionHandler: Send + Sync + 'static {
    fn request(&mut self, incoming: &Incoming) -> bool;
}

pub struct AlwaysAcceptIncoming;

impl IncomingConnectionHandler for AlwaysAcceptIncoming {
    fn request(&mut self, _incoming: &Incoming) -> bool {
        true
    }
}

impl AlwaysAcceptIncoming {
    pub fn new() -> Box<dyn IncomingConnectionHandler> {
        Box::new(AlwaysAcceptIncoming)
    }
}

pub struct AlwaysRejectIncoming;

impl IncomingConnectionHandler for AlwaysRejectIncoming {
    fn request(&mut self, _incoming: &Incoming) -> bool {
        false
    }
}

impl AlwaysRejectIncoming {
    pub fn new() -> Box<dyn IncomingConnectionHandler> {
        Box::new(AlwaysRejectIncoming)
    }
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
