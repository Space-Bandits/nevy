//! WebTransport endpoint implementation.

use std::{
    any::Any,
    collections::VecDeque,
    net::{SocketAddr, ToSocketAddrs, UdpSocket},
    sync::Arc,
    time::Instant,
};

use bevy::{platform::collections::HashMap, prelude::*};
use bytes::Bytes;
use log::{error, warn};
use quinn_proto::{
    ClientConfig, ConnectError, ConnectionError, ConnectionHandle, DatagramEvent, Dir, Incoming,
    VarInt,
};
use quinn_udp::{UdpSockRef, UdpSocketState};

use crate::{
    Connection, ConnectionOf, ConnectionStatus, Endpoint, NoConnectionError, Transport,
    protocols::quic::{
        connection::{CloseFlagState, QuicConnectionContext, QuicConnectionState},
        udp_transmit,
    },
};

use super::{
    connection::WebTransportConnectionContext,
    h3::{
        encode_connect_request, encode_connect_response, parse_connect_request,
        parse_connect_response, H3ControlStream,
    },
    session::{SessionState, WebTransportSession},
};

/// Configuration for initiating a WebTransport connection.
#[derive(Component, Clone)]
pub struct WebTransportConnectionConfig {
    /// Underlying QUIC client configuration.
    pub client_config: ClientConfig,
    /// Server address to connect to.
    pub address: SocketAddr,
    /// Server name (for TLS SNI).
    pub server_name: String,
    /// WebTransport session path (e.g., "/game").
    pub path: String,
}

/// Reason for WebTransport connection closure.
#[derive(Component, Deref)]
pub struct WebTransportConnectionClosedReason(pub ConnectionError);

/// Reason for WebTransport connection failure.
#[derive(Component, Deref)]
pub struct WebTransportConnectionFailedReason(pub ConnectError);

/// Incoming WebTransport connection waiting to be accepted or rejected.
#[derive(Component)]
pub struct IncomingWebTransportConnection {
    /// The endpoint entity this connection is coming from.
    pub endpoint_entity: Entity,
    /// The underlying QUIC incoming connection.
    pub incoming: Option<Incoming>,
}

/// Internal state for a WebTransport connection.
pub(crate) struct WebTransportConnectionState {
    /// The underlying QUIC connection state.
    pub quic: QuicConnectionState,
    /// H3 control stream state (send).
    pub control_stream_send: Option<quinn_proto::StreamId>,
    /// H3 control stream state (recv).
    pub control_stream_recv: Option<H3ControlStream>,
    /// WebTransport session.
    pub session: WebTransportSession,
    /// Pending CONNECT request stream (client).
    pub connect_stream: Option<quinn_proto::StreamId>,
    /// Path requested by client (server stores this after parsing CONNECT).
    pub requested_path: Option<String>,
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
        let Some(endpoint) = endpoint.as_transport::<WebTransportEndpoint>() else {
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
        Option<&WebTransportConnectionConfig>,
        Option<&mut IncomingWebTransportConnection>,
    )>,
) -> Result {
    let connection_entity = insert.entity;

    let (connection_of, config, incoming) = connection_q.get_mut(connection_entity)?;

    let mut endpoint = endpoint_q.get_mut(**connection_of)?;

    let Some(endpoint) = endpoint.as_transport::<WebTransportEndpoint>() else {
        return Ok(());
    };

    if let Some(mut incoming) = incoming {
        commands
            .entity(connection_entity)
            .remove::<IncomingWebTransportConnection>();

        let Some(incoming) = incoming.incoming.take() else {
            error!("User tried to accept an incoming WebTransport connection twice.");
            return Ok(());
        };

        match endpoint.endpoint.accept(
            incoming,
            Instant::now(),
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

                let quic_state = QuicConnectionState {
                    connection_entity,
                    connection,
                    stream_events: VecDeque::new(),
                    close: CloseFlagState::None,
                    datagrams_queue_offset: 0,
                    datagrams: VecDeque::new(),
                };

                endpoint.connections.insert(
                    connection_handle,
                    WebTransportConnectionState {
                        quic: quic_state,
                        control_stream_send: None,
                        control_stream_recv: Some(H3ControlStream::new()),
                        session: WebTransportSession::new(),
                        connect_stream: None,
                        requested_path: None,
                    },
                );
                // Note: Control stream and SETTINGS will be sent when Connected event fires
            }
        };
    } else {
        let connection_config = config.ok_or_else(|| {
            commands.entity(connection_entity).remove::<ConnectionOf>();
            format!(
                "No WebTransport config provided for connection {}. Removing `ConnectionOf`",
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
            Err(err) => {
                warn!(
                    "Failed to open WebTransport connection to {} {}",
                    connection_config.address, connection_config.server_name
                );

                commands.entity(connection_entity).insert((
                    ConnectionStatus::Failed,
                    WebTransportConnectionFailedReason(err),
                ));

                return Ok(());
            }
        };

        endpoint
            .connection_handles
            .insert(connection_entity, connection_handle);

        let quic_state = QuicConnectionState {
            connection_entity,
            connection,
            stream_events: VecDeque::new(),
            close: CloseFlagState::None,
            datagrams_queue_offset: 0,
            datagrams: VecDeque::new(),
        };

        let mut session = WebTransportSession::new();
        session.state = SessionState::AwaitingSettings;

        endpoint.connections.insert(
            connection_handle,
            WebTransportConnectionState {
                quic: quic_state,
                control_stream_send: None,
                control_stream_recv: Some(H3ControlStream::new()),
                session,
                connect_stream: None,
                requested_path: Some(connection_config.path.clone()),
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

    let Some(endpoint) = endpoint.as_transport::<WebTransportEndpoint>() else {
        return Ok(());
    };

    if let Some((connection_handle, already_drained)) =
        endpoint.connections.iter().find_map(|(handle, conn)| {
            if conn.quic.connection_entity != connection_entity {
                return None;
            }
            Some((*handle, conn.quic.connection.is_drained()))
        })
    {
        if !already_drained {
            endpoint
                .endpoint
                .handle_event(connection_handle, quinn_proto::EndpointEvent::drained());
        }

        endpoint.connections.remove(&connection_handle);
        endpoint.connection_handles.remove(&connection_entity);
    }

    Ok(())
}

pub(super) fn refuse_connections(
    replace: On<Replace, IncomingWebTransportConnection>,
    mut endpoint_q: Query<&mut Endpoint>,
    mut connection_q: Query<&mut IncomingWebTransportConnection>,
) -> Result {
    let connection_entity = replace.entity;

    let mut incoming_component = connection_q.get_mut(connection_entity)?;

    if let Some(incoming) = incoming_component.incoming.take() {
        let mut endpoint = endpoint_q.get_mut(incoming_component.endpoint_entity)?;

        let endpoint = endpoint.as_transport::<WebTransportEndpoint>().ok_or(
            "An incoming WebTransport connection should always point to a WebTransport endpoint",
        )?;

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

/// WebTransport endpoint implementation.
///
/// Wraps a QUIC endpoint and adds HTTP/3 + WebTransport protocol handling.
pub struct WebTransportEndpoint {
    endpoint: quinn_proto::Endpoint,
    connection_handles: HashMap<Entity, ConnectionHandle>,
    connections: HashMap<ConnectionHandle, WebTransportConnectionState>,
    socket: UdpSocket,
    socket_state: UdpSocketState,
    local_addr: SocketAddr,
    endpoint_config: quinn_proto::EndpointConfig,
    server_config: Option<quinn_proto::ServerConfig>,
    recv_buffer: Vec<u8>,
    send_buffer: Vec<u8>,
}

impl WebTransportEndpoint {
    /// Create a new WebTransport endpoint.
    ///
    /// The ALPN protocol will be set to "h3" for WebTransport.
    pub fn new(
        bind_addr: impl ToSocketAddrs,
        endpoint_config: quinn_proto::EndpointConfig,
        server_config: Option<quinn_proto::ServerConfig>,
    ) -> Result<Endpoint, std::io::Error> {
        let socket = UdpSocket::bind(bind_addr)?;
        socket.set_nonblocking(true)?;
        let socket_state = UdpSocketState::new(UdpSockRef::from(&socket))?;
        let local_addr = socket.local_addr()?;
        log::info!("WebTransport endpoint bound to {}", local_addr);

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

    /// Get the local address this endpoint is bound to.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    fn update(&mut self, mut context: EndpointUpdateContext) {
        log::trace!("WebTransport endpoint update tick");
        self.receive_datagrams(&mut context);
        self.update_connections(&mut context);
        self.process_h3_protocol(&mut context);
    }

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
            .map(std::io::IoSliceMut::new);

        let mut buffer_chunks: [std::io::IoSliceMut; quinn_udp::BATCH_SIZE] =
            std::array::from_fn(|_| buffer_chunks.next().unwrap());

        let mut metas = [quinn_udp::RecvMeta::default(); quinn_udp::BATCH_SIZE];

        loop {
            match self.socket_state.recv(
                UdpSockRef::from(&self.socket),
                &mut buffer_chunks,
                &mut metas,
            ) {
                Ok(datagram_count) => {
                    log::debug!("Received {} datagrams", datagram_count);
                    self.handle_packet(datagram_count, &buffer_chunks, &metas, context);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {}
                Err(other) => {
                    error!(
                        "Received an unexpected error while receiving endpoint datagrams: {:?}",
                        other
                    );
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
        buffer_chunks: &[std::io::IoSliceMut; quinn_udp::BATCH_SIZE],
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

                let datagram_event = self.endpoint.handle(
                    Instant::now(),
                    meta.addr,
                    meta.dst_ip,
                    ecn,
                    data.into(),
                    &mut self.send_buffer,
                );

                log::debug!("Received packet from {}, {} bytes, event: {:?}", meta.addr, data.len(), datagram_event.as_ref().map(|e| match e {
                    DatagramEvent::NewConnection(_) => "NewConnection",
                    DatagramEvent::ConnectionEvent(_, _) => "ConnectionEvent",
                    DatagramEvent::Response(_) => "Response",
                }));

                // Check if there's data in send_buffer to send back
                if !self.send_buffer.is_empty() {
                    log::debug!("Send buffer has {} bytes after handle", self.send_buffer.len());
                }

                let Some(datagram_event) = datagram_event else {
                    continue;
                };

                self.handle_datagram_event(datagram_event, context);
            }
        }
    }

    fn handle_datagram_event(
        &mut self,
        event: DatagramEvent,
        context: &mut EndpointUpdateContext,
    ) {
        let transmit = match event {
            DatagramEvent::NewConnection(incoming) => {
                self.handle_incoming_connection(incoming, context)
            }
            DatagramEvent::ConnectionEvent(connection_handle, event) => {
                let Some(connection) = self.connections.get_mut(&connection_handle) else {
                    error!("Connection event for unknown connection");
                    return;
                };

                connection.quic.connection.handle_event(event);
                None
            }
            DatagramEvent::Response(transmit) => Some(transmit),
        };

        if let Some(transmit) = transmit {
            let _ = self.socket_state.send(
                UdpSockRef::from(&self.socket),
                &udp_transmit(&transmit, &self.send_buffer),
            );
        }
    }

    fn handle_incoming_connection(
        &mut self,
        incoming: Incoming,
        context: &mut EndpointUpdateContext,
    ) -> Option<quinn_proto::Transmit> {
        log::info!("Incoming connection from {}", incoming.remote_address());

        if self.server_config.is_none() {
            error!(
                "Remote address {} attempted to connect but endpoint is not configured as server",
                incoming.remote_address()
            );
            return Some(self.endpoint.refuse(incoming, &mut self.send_buffer));
        }

        context.commands.spawn(IncomingWebTransportConnection {
            endpoint_entity: context.endpoint_entity,
            incoming: Some(incoming),
        });

        None
    }

    fn setup_server_control_stream(&mut self, connection_handle: ConnectionHandle) {
        let Some(conn) = self.connections.get_mut(&connection_handle) else {
            return;
        };

        // Open control stream and send SETTINGS
        if let Some(stream_id) = conn.quic.connection.streams().open(Dir::Uni) {
            let settings = H3ControlStream::create_settings_frame();
            let _ = conn.quic.connection.send_stream(stream_id).write(&settings);
            conn.control_stream_send = Some(stream_id);
        }
    }

    fn update_connections(&mut self, context: &mut EndpointUpdateContext) {
        for (&connection_handle, conn) in self.connections.iter_mut() {
            // Handle close flag
            if let CloseFlagState::Sent = conn.quic.close {
                conn.quic.close = CloseFlagState::Received;
                conn.quic
                    .connection
                    .close(Instant::now(), VarInt::from_u32(0), Bytes::new());

                context
                    .commands
                    .entity(conn.quic.connection_entity)
                    .insert(ConnectionStatus::Closed);
            }

            // Transmit packets
            QuicConnectionContext {
                connection: &mut conn.quic,
                send_buffer: &mut self.send_buffer,
                max_datagrams: self.socket_state.gro_segments(),
                socket: &self.socket,
                socket_state: &self.socket_state,
            }
            .transmit_packets();

            // Handle timeouts
            let now = Instant::now();
            while let Some(deadline) = conn.quic.connection.poll_timeout() {
                if deadline <= now {
                    conn.quic.connection.handle_timeout(now);
                } else {
                    break;
                }
            }

            // Handle endpoint events
            while let Some(endpoint_event) = conn.quic.connection.poll_endpoint_events() {
                if let Some(connection_event) =
                    self.endpoint.handle_event(connection_handle, endpoint_event)
                {
                    conn.quic.connection.handle_event(connection_event);
                }
            }

            // Handle application events
            while let Some(app_event) = conn.quic.connection.poll() {
                log::debug!("Connection {} got event: {:?}", conn.quic.connection_entity,
                    match &app_event {
                        quinn_proto::Event::HandshakeDataReady => "HandshakeDataReady",
                        quinn_proto::Event::Connected => "Connected",
                        quinn_proto::Event::ConnectionLost { .. } => "ConnectionLost",
                        quinn_proto::Event::Stream(_) => "Stream",
                        quinn_proto::Event::DatagramReceived => "DatagramReceived",
                        quinn_proto::Event::DatagramsUnblocked => "DatagramsUnblocked",
                    });
                match app_event {
                    quinn_proto::Event::HandshakeDataReady => {
                        log::debug!("HandshakeDataReady for {}", conn.quic.connection_entity);
                    }
                    quinn_proto::Event::Connected => {
                        log::info!("QUIC Connected event fired for {}", conn.quic.connection_entity);
                        // Both client and server need to send H3 SETTINGS on control stream
                        if conn.control_stream_send.is_none() {
                            log::debug!("Attempting to open control stream...");
                            if let Some(stream_id) = conn.quic.connection.streams().open(Dir::Uni) {
                                let settings = H3ControlStream::create_settings_frame();
                                log::info!("Sending H3 SETTINGS on stream {:?}, {} bytes", stream_id, settings.len());
                                let _ = conn.quic.connection.send_stream(stream_id).write(&settings);
                                conn.control_stream_send = Some(stream_id);
                            } else {
                                log::warn!("Failed to open control stream - streams().open returned None");
                            }
                        } else {
                            log::debug!("Control stream already exists: {:?}", conn.control_stream_send);
                        }
                    }
                    quinn_proto::Event::ConnectionLost { reason } => {
                        log::error!("Connection {} lost: {:?}", conn.quic.connection_entity, reason);
                        context.commands.entity(conn.quic.connection_entity).insert((
                            ConnectionStatus::Closed,
                            WebTransportConnectionClosedReason(reason),
                        ));
                    }
                    quinn_proto::Event::Stream(event) => {
                        conn.quic.stream_events.push_back(event);
                    }
                    quinn_proto::Event::DatagramReceived => {}
                    quinn_proto::Event::DatagramsUnblocked => {}
                }
            }
        }
    }

    fn process_h3_protocol(&mut self, context: &mut EndpointUpdateContext) {
        let connection_handles: Vec<_> = self.connections.keys().copied().collect();

        for connection_handle in connection_handles {
            let Some(conn) = self.connections.get_mut(&connection_handle) else {
                continue;
            };

            // Accept incoming unidirectional streams (control stream from peer)
            let accepted_uni: Vec<_> = std::iter::from_fn(|| {
                conn.quic.connection.streams().accept(Dir::Uni)
            }).collect();

            if !accepted_uni.is_empty() {
                log::info!("Accepted {} incoming uni streams for {}", accepted_uni.len(), conn.quic.connection_entity);
            }

            for stream_id in accepted_uni {
                log::info!("Processing incoming uni stream {:?}", stream_id);
                // This should be the peer's control stream
                if conn.control_stream_recv.is_some() {
                    // Read data from stream first
                    let data = {
                        let mut stream = conn.quic.connection.recv_stream(stream_id);
                        let mut data = Vec::new();
                        if let Ok(mut chunks) = stream.read(true) {
                            while let Ok(Some(chunk)) = chunks.next(usize::MAX) {
                                data.extend_from_slice(&chunk.bytes);
                            }
                        }
                        data
                    };

                    log::info!("Read {} bytes from uni stream {:?}", data.len(), stream_id);
                    if !data.is_empty() {
                        if let Some(ref mut control) = conn.control_stream_recv {
                            let result = control.process_received(Bytes::from(data));
                            log::info!("Control stream process result: {:?}, settings_received: {}", result, control.settings_received);

                            if control.settings_received && control.peer_settings.supports_webtransport() {
                                log::info!("Peer supports WebTransport! Transitioning to SettingsReceived");
                                conn.session.state = SessionState::SettingsReceived;

                                // Client: send CONNECT request
                                if let Some(ref path) = conn.requested_path.clone() {
                                    if conn.connect_stream.is_none() {
                                        if let Some(bidi_stream) =
                                            conn.quic.connection.streams().open(Dir::Bi)
                                        {
                                            let authority = conn
                                                .quic
                                                .connection
                                                .remote_address()
                                                .to_string();

                                            if let Ok(connect_request) =
                                                encode_connect_request(&authority, &path)
                                            {
                                                let _ = conn
                                                    .quic
                                                    .connection
                                                    .send_stream(bidi_stream)
                                                    .write(&connect_request);
                                                conn.connect_stream = Some(bidi_stream);
                                                conn.session.state =
                                                    SessionState::AwaitingConnectResponse;
                                                conn.session.request_stream_id =
                                                    Some(bidi_stream);
                                                conn.session.session_id =
                                                    bidi_stream.index() / 4;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Accept incoming bidirectional streams for CONNECT request (server only, before session established)
            // After session is established, bidi streams are data streams for the user application
            if conn.requested_path.is_none() && conn.session.state == SessionState::SettingsReceived {
                let accepted_bidi: Vec<_> = std::iter::from_fn(|| {
                    conn.quic.connection.streams().accept(Dir::Bi)
                }).collect();

                if !accepted_bidi.is_empty() {
                    log::info!("Accepted {} incoming bidi streams for CONNECT", accepted_bidi.len());
                }

                for stream_id in accepted_bidi {
                    log::info!("Processing CONNECT request on bidi stream {:?}", stream_id);
                    // Read data from stream first
                    let data = {
                        let mut stream = conn.quic.connection.recv_stream(stream_id);
                        let mut data = Vec::new();
                        if let Ok(mut chunks) = stream.read(true) {
                            while let Ok(Some(chunk)) = chunks.next(usize::MAX) {
                                data.extend_from_slice(&chunk.bytes);
                            }
                        }
                        data
                    };

                    log::info!("Read {} bytes from bidi stream {:?}: {:?}", data.len(), stream_id,
                        String::from_utf8_lossy(&data[..data.len().min(100)]));
                    if !data.is_empty() {
                        match parse_connect_request(&data) {
                            Ok((_authority, path, protocol)) => {
                                log::info!("Parsed CONNECT request: authority={}, path={}, protocol={}", _authority, path, protocol);
                                if protocol == "webtransport" {
                                    // Send 200 OK response
                                    if let Ok(response) = encode_connect_response() {
                                        log::info!("Sending 200 OK response, {} bytes", response.len());
                                        let _ = conn
                                            .quic
                                            .connection
                                            .send_stream(stream_id)
                                            .write(&response);
                                        conn.session.state = SessionState::Established;
                                        conn.session.request_stream_id = Some(stream_id);
                                        conn.session.session_id = stream_id.index() / 4;
                                        conn.requested_path = Some(path);

                                        context
                                            .commands
                                            .entity(conn.quic.connection_entity)
                                            .insert(ConnectionStatus::Established);
                                        log::info!("WebTransport session established!");
                                    }
                                }
                            }
                            Err(e) => {
                                log::warn!("Failed to parse CONNECT request: {:?}", e);
                            }
                        }
                    }
                }
            }

            // Client: check for CONNECT response
            if conn.session.state == SessionState::AwaitingConnectResponse {
                if let Some(connect_stream) = conn.connect_stream {
                    let mut stream = conn.quic.connection.recv_stream(connect_stream);
                    if let Ok(mut chunks) = stream.read(true) {
                        let mut data = Vec::new();
                        while let Ok(Some(chunk)) = chunks.next(usize::MAX) {
                            data.extend_from_slice(&chunk.bytes);
                        }

                        if !data.is_empty() {
                            if let Ok(status) = parse_connect_response(&data) {
                                if status == 200 {
                                    conn.session.state = SessionState::Established;

                                    context
                                        .commands
                                        .entity(conn.quic.connection_entity)
                                        .insert(ConnectionStatus::Established);
                                } else {
                                    // Non-200 response, connection failed
                                    conn.session.state = SessionState::Closed;
                                    context
                                        .commands
                                        .entity(conn.quic.connection_entity)
                                        .insert(ConnectionStatus::Failed);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

impl Transport for WebTransportEndpoint {
    fn as_any<'a>(&'a mut self) -> &'a mut dyn Any {
        self
    }

    fn get_connection<'a>(
        &'a mut self,
        connection: Entity,
    ) -> Result<Connection<'a>, NoConnectionError> {
        let handle = self
            .connection_handles
            .get(&connection)
            .ok_or(NoConnectionError)?;

        let conn = self.connections.get_mut(handle).ok_or(NoConnectionError)?;

        // Only return connection context if session is established
        if conn.session.state != SessionState::Established {
            return Err(NoConnectionError);
        }

        Ok(Connection(Box::new(WebTransportConnectionContext {
            connection: &mut conn.quic,
            session: &mut conn.session,
            send_buffer: &mut self.send_buffer,
            max_datagrams: self.socket_state.gro_segments(),
            socket: &self.socket,
            socket_state: &self.socket_state,
        })))
    }
}
