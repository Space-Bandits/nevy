use bevy::{
    log::{Level, LogPlugin},
    prelude::*,
};
use nevy::prelude::*;

fn main() {
    let mut app = App::new();

    app.add_plugins(MinimalPlugins);
    app.add_plugins(LogPlugin {
        level: Level::DEBUG,
        ..default()
    });
    app.add_plugins(WebTransportPlugin::default());

    app.add_systems(Startup, setup);
    app.add_observer(accept_connections);
    app.add_observer(log_status_changes);
    app.add_systems(Update, (accept_streams, read_streams));

    app.run();
}

/// Spawns an endpoint that can accept WebTransport connections.
fn setup(mut commands: Commands) {
    let (server_config, cert_hash) = create_server_config();

    // Print certificate hash for Chrome
    println!("\n==============================================");
    println!("WebTransport Server starting on 0.0.0.0:4433");
    println!("==============================================\n");
    println!("Certificate SHA-256 hash (base64):");
    println!("  {}", cert_hash);
    println!("\nTo connect from Chrome, use this JavaScript:\n");
    println!(r#"const transport = new WebTransport("https://localhost:4433/", {{
  serverCertificateHashes: [{{
    algorithm: "sha-256",
    value: Uint8Array.from(atob("{}"), c => c.charCodeAt(0))
  }}]
}});
await transport.ready;
console.log("Connected!");

// Open a bidirectional stream
const stream = await transport.createBidirectionalStream();
const writer = stream.writable.getWriter();
const reader = stream.readable.getReader();

// Send a message
await writer.write(new TextEncoder().encode("Hello from browser!"));
await writer.close();

// Read response
const {{ value }} = await reader.read();
console.log("Received:", new TextDecoder().decode(value));"#, cert_hash);
    println!("\n==============================================\n");

    commands.spawn(
        WebTransportEndpoint::new(
            "0.0.0.0:4433",
            quinn_proto::EndpointConfig::default(),
            Some(server_config),
        )
        .unwrap(),
    );
}

/// Accepts WebTransport connections.
fn accept_connections(
    insert: On<Insert, IncomingWebTransportConnection>,
    mut commands: Commands,
    connection_q: Query<&IncomingWebTransportConnection>,
) -> Result {
    let &IncomingWebTransportConnection {
        endpoint_entity, ..
    } = connection_q.get(insert.entity)?;

    info!("Accepting incoming WebTransport connection");

    commands
        .entity(insert.entity)
        .insert((ConnectionOf(endpoint_entity), ConnectionStreams::default()));

    Ok(())
}

/// Records data received on streams.
#[derive(Component, Default, Deref, DerefMut)]
struct ConnectionStreams(Vec<(Stream, Vec<u8>)>);

fn accept_streams(
    mut connection_q: Query<(Entity, &ConnectionOf, &ConnectionStatus, &mut ConnectionStreams)>,
    mut endpoint_q: Query<&mut Endpoint>,
) -> Result {
    for (connection_entity, &ConnectionOf(endpoint_entity), status, mut streams) in &mut connection_q {
        // Only try to access established connections
        if !matches!(status, ConnectionStatus::Established) {
            continue;
        }

        let mut endpoint = endpoint_q.get_mut(endpoint_entity)?;

        let Ok(mut connection) = endpoint.get_connection(connection_entity) else {
            continue;
        };

        while let Some((stream, _)) = connection.accept_stream() {
            info!("Accepted new stream from {}", connection_entity);
            streams.push((stream, Vec::new()));
        }
    }

    Ok(())
}

fn read_streams(
    mut connection_q: Query<(Entity, &ConnectionOf, &ConnectionStatus, &mut ConnectionStreams)>,
    mut endpoint_q: Query<&mut Endpoint>,
) -> Result {
    for (connection_entity, &ConnectionOf(endpoint_entity), status, mut streams) in &mut connection_q {
        // Only try to access established connections
        if !matches!(status, ConnectionStatus::Established) {
            continue;
        }

        let mut endpoint = endpoint_q.get_mut(endpoint_entity)?;

        let Ok(mut connection) = endpoint.get_connection(connection_entity) else {
            continue;
        };

        let mut closed_streams = Vec::with_capacity(streams.len());

        for &mut (ref stream, ref mut buffer) in streams.iter_mut() {
            closed_streams.push(loop {
                match connection.read(stream)? {
                    Ok(chunk) => {
                        info!("Received {} bytes from {}", chunk.len(), connection_entity);

                        // Echo immediately when we receive data
                        let msg = String::from_utf8_lossy(&chunk);
                        info!("Message: {:?}", msg);

                        let response = format!("Echo: {}", msg);
                        info!("Sending echo response: {:?}", response);
                        if let Err(e) = connection.write(stream, bytes::Bytes::from(response), false) {
                            warn!("Failed to echo response: {:?}", e);
                        }

                        buffer.extend(chunk);
                    }
                    Err(StreamReadError::Blocked) => break true,
                    Err(StreamReadError::Closed) => break false,
                }
            });
        }

        let mut closed_streams = closed_streams.into_iter();
        streams.retain(|(stream, _buffer)| {
            let keep = closed_streams.next().unwrap_or(false);

            if !keep {
                info!("Stream closed, cleaning up");
                let _ = connection.close_send_stream(stream, true);
            }

            keep
        });
    }

    Ok(())
}

fn log_status_changes(
    insert: On<Insert, ConnectionStatus>,
    status_q: Query<&ConnectionStatus>,
) -> Result {
    let status = status_q.get(insert.entity)?;
    info!(
        "Connection {} status changed to {:?}",
        insert.entity, status
    );
    Ok(())
}

fn create_server_config() -> (quinn_proto::ServerConfig, String) {
    use ring::digest::{SHA256, digest};
    use base64::Engine;
    use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256, ExtendedKeyUsagePurpose};

    // Chrome requires certificates for serverCertificateHashes to:
    // 1. Be valid for max 14 days
    // 2. Use ECDSA P-256 (or P-384, Ed25519)
    // 3. Have serverAuth extended key usage
    let mut params = CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()]).unwrap();

    // Set validity to 14 days (Chrome's max for serverCertificateHashes)
    params.not_before = time::OffsetDateTime::now_utc();
    params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(14);

    // Add extended key usage for TLS server authentication
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];

    // Use ECDSA P-256
    let key_pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
    let cert = params.self_signed(&key_pair).unwrap();

    let cert_der = cert.der().clone();
    let key = rustls::pki_types::PrivateKeyDer::try_from(key_pair.serialize_der()).unwrap();

    // Calculate SHA-256 hash of the certificate
    let hash = digest(&SHA256, &cert_der);
    let cert_hash = base64::engine::general_purpose::STANDARD.encode(hash.as_ref());

    // Also print raw hex for debugging
    let hex_hash: String = hash.as_ref().iter().map(|b| format!("{:02x}", b)).collect();
    eprintln!("Certificate hash (hex): {}", hex_hash);
    eprintln!("Certificate DER length: {} bytes", cert_der.len());

    // Write certificate to temp file for verification with openssl
    let cert_path = "/tmp/webtransport_cert.der";
    std::fs::write(cert_path, &cert_der).expect("Failed to write certificate");
    eprintln!("Certificate written to: {}", cert_path);
    eprintln!("Verify with: openssl x509 -inform DER -in {} -noout -fingerprint -sha256", cert_path);

    let mut tls_config = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert_der], key)
    .unwrap();

    tls_config.max_early_data_size = u32::MAX;
    tls_config.alpn_protocols = vec![b"h3".to_vec()];

    let quic_tls_config =
        quinn_proto::crypto::rustls::QuicServerConfig::try_from(tls_config).unwrap();

    let mut server_config =
        quinn_proto::ServerConfig::with_crypto(std::sync::Arc::new(quic_tls_config));

    let mut transport_config = quinn_proto::TransportConfig::default();
    transport_config.max_idle_timeout(Some(std::time::Duration::from_secs(30).try_into().unwrap()));
    transport_config.keep_alive_interval(Some(std::time::Duration::from_millis(200)));
    // Enable datagrams for WebTransport
    transport_config.datagram_receive_buffer_size(Some(65536));

    server_config.transport = std::sync::Arc::new(transport_config);

    (server_config, cert_hash)
}
