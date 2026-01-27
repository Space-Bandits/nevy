//! Simple test to check if QUIC connection works to the WebTransport server

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn main() {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
    socket.set_nonblocking(true).unwrap();

    let socket_state = quinn_udp::UdpSocketState::new(quinn_udp::UdpSockRef::from(&socket)).unwrap();

    let mut endpoint = quinn_proto::Endpoint::new(
        Arc::new(quinn_proto::EndpointConfig::default()),
        None,
        true,
        None,
    );

    let client_config = create_client_config();
    let server_addr: SocketAddr = "127.0.0.1:4433".parse().unwrap();

    let mut send_buffer = Vec::new();

    let (connection_handle, connection) = endpoint
        .connect(Instant::now(), client_config, server_addr, "localhost")
        .expect("Failed to initiate connection");

    println!("Initiated connection to {}", server_addr);

    let mut connection = connection;
    let start = Instant::now();

    // Simple poll loop
    loop {
        if start.elapsed() > Duration::from_secs(5) {
            println!("Timeout after 5 seconds");
            break;
        }

        // Send any pending transmits
        while let Some(transmit) = connection.poll_transmit(Instant::now(), 1400, &mut send_buffer) {
            let udp_transmit = quinn_udp::Transmit {
                destination: transmit.destination,
                ecn: transmit.ecn.map(|ecn| match ecn {
                    quinn_proto::EcnCodepoint::Ect0 => quinn_udp::EcnCodepoint::Ect0,
                    quinn_proto::EcnCodepoint::Ect1 => quinn_udp::EcnCodepoint::Ect1,
                    quinn_proto::EcnCodepoint::Ce => quinn_udp::EcnCodepoint::Ce,
                }),
                contents: &send_buffer[0..transmit.size],
                segment_size: transmit.segment_size,
                src_ip: transmit.src_ip,
            };

            match socket_state.send(quinn_udp::UdpSockRef::from(&socket), &udp_transmit) {
                Ok(_) => println!("Sent {} bytes to {}", transmit.size, transmit.destination),
                Err(e) => println!("Send error: {}", e),
            }
            send_buffer.clear();
        }

        // Try to receive
        let mut recv_buf = vec![0u8; 65536];
        let mut recv_slices = [std::io::IoSliceMut::new(&mut recv_buf)];
        let mut metas = [quinn_udp::RecvMeta::default()];

        match socket_state.recv(quinn_udp::UdpSockRef::from(&socket), &mut recv_slices, &mut metas) {
            Ok(count) => {
                for i in 0..count {
                    let meta = &metas[i];
                    let data = &recv_buf[..meta.len];
                    println!("Received {} bytes from {}", meta.len, meta.addr);

                    let ecn = meta.ecn.map(|ecn| match ecn {
                        quinn_udp::EcnCodepoint::Ect0 => quinn_proto::EcnCodepoint::Ect0,
                        quinn_udp::EcnCodepoint::Ect1 => quinn_proto::EcnCodepoint::Ect1,
                        quinn_udp::EcnCodepoint::Ce => quinn_proto::EcnCodepoint::Ce,
                    });

                    if let Some(event) = endpoint.handle(
                        Instant::now(),
                        meta.addr,
                        meta.dst_ip,
                        ecn,
                        data.into(),
                        &mut send_buffer,
                    ) {
                        match event {
                            quinn_proto::DatagramEvent::NewConnection(_) => {
                                println!("Unexpected new connection event");
                            }
                            quinn_proto::DatagramEvent::ConnectionEvent(handle, event) => {
                                if handle == connection_handle {
                                    connection.handle_event(event);
                                }
                            }
                            quinn_proto::DatagramEvent::Response(transmit) => {
                                println!("Response transmit to send");
                            }
                        }
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => println!("Recv error: {}", e),
        }

        // Check connection state
        while let Some(event) = connection.poll() {
            println!("Connection event: {:?}", event);
            match event {
                quinn_proto::Event::HandshakeDataReady => {
                    println!("Handshake data ready!");
                }
                quinn_proto::Event::Connected => {
                    println!("Connected successfully!");
                    return;
                }
                quinn_proto::Event::ConnectionLost { reason } => {
                    println!("Connection lost: {:?}", reason);
                    return;
                }
                _ => {}
            }
        }

        // Handle timeouts
        if let Some(timeout) = connection.poll_timeout() {
            if timeout <= Instant::now() {
                connection.handle_timeout(Instant::now());
            }
        }

        std::thread::sleep(Duration::from_millis(10));
    }
}

fn create_client_config() -> quinn_proto::ClientConfig {
    #[derive(Debug)]
    struct SkipVerify;

    impl rustls::client::danger::ServerCertVerifier for SkipVerify {
        fn verify_server_cert(
            &self,
            _end_entity: &rustls::pki_types::CertificateDer<'_>,
            _intermediates: &[rustls::pki_types::CertificateDer<'_>],
            _server_name: &rustls::pki_types::ServerName<'_>,
            _ocsp_response: &[u8],
            _now: rustls::pki_types::UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &rustls::pki_types::CertificateDer<'_>,
            _dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &rustls::pki_types::CertificateDer<'_>,
            _dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            vec![
                rustls::SignatureScheme::RSA_PKCS1_SHA256,
                rustls::SignatureScheme::RSA_PKCS1_SHA384,
                rustls::SignatureScheme::RSA_PKCS1_SHA512,
                rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
                rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
                rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
                rustls::SignatureScheme::RSA_PSS_SHA256,
                rustls::SignatureScheme::RSA_PSS_SHA384,
                rustls::SignatureScheme::RSA_PSS_SHA512,
                rustls::SignatureScheme::ED25519,
            ]
        }
    }

    let mut tls_config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(SkipVerify))
    .with_no_client_auth();

    tls_config.alpn_protocols = vec![b"h3".to_vec()];

    let quic_config = quinn_proto::crypto::rustls::QuicClientConfig::try_from(tls_config).unwrap();
    quinn_proto::ClientConfig::new(Arc::new(quic_config))
}
