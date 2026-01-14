use bevy::{
    log::{Level, LogPlugin},
    prelude::*,
};
use messages::HelloServer;
use nevy::prelude::*;

fn main() {
    let mut app = App::new();

    app.add_plugins(MinimalPlugins);
    app.add_plugins(LogPlugin {
        level: Level::DEBUG,
        ..default()
    });
    app.add_plugins(NevyPlugins::default());

    messages::build(&mut app);

    app.add_systems(Startup, setup);
    app.add_observer(log_status_changes);
    app.add_systems(Update, send_messages);

    app.run();
}

/// Creates an endpoint and initiates a connection.
fn setup(mut commands: Commands) {
    let endpoint_entity = commands
        .spawn((
            ConnectionProtocol::<()>::default(),
            QuicEndpoint::new("0.0.0.0:0", quinn_proto::EndpointConfig::default(), None).unwrap(),
        ))
        .id();

    commands.spawn((
        ConnectionOf(endpoint_entity),
        QuicConnectionConfig {
            client_config: create_connection_config(),
            address: "127.0.0.1:27518".parse().unwrap(),
            server_name: "example.server".to_string(),
        },
    ));
}

fn send_messages(
    connection_q: Query<(Entity, &ConnectionStatus), Changed<ConnectionStatus>>,
    mut sender: LocalMessageSenderUnord,
) -> Result {
    sender.flush()?;

    for (connection_entity, status) in &connection_q {
        let ConnectionStatus::Established = status else {
            continue;
        };

        sender.write(
            connection_entity,
            true,
            &HelloServer {
                data: "Hello Server!".into(),
            },
        )?;

        info!("Wrote a message");
    }

    Ok(())
}

/// Logs changes to the connection status
fn log_status_changes(
    insert: On<Insert, ConnectionStatus>,
    status_q: Query<&ConnectionStatus>,
) -> Result {
    let status = status_q.get(insert.entity)?;
    info!("Connection status changed to {:?}", status);
    Ok(())
}

fn create_connection_config() -> quinn_proto::ClientConfig {
    // some day I need to figure out how to do tls properly
    // someone help me

    #[derive(Debug)]
    struct AlwaysVerify;

    impl rustls::client::danger::ServerCertVerifier for AlwaysVerify {
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
                rustls::SignatureScheme::RSA_PKCS1_SHA512,
                rustls::SignatureScheme::RSA_PKCS1_SHA384,
                rustls::SignatureScheme::RSA_PKCS1_SHA1,
                rustls::SignatureScheme::RSA_PSS_SHA256,
                rustls::SignatureScheme::RSA_PSS_SHA384,
                rustls::SignatureScheme::RSA_PSS_SHA512,
                rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
                rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
                rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
                rustls::SignatureScheme::ED25519,
                rustls::SignatureScheme::ED448,
                rustls::SignatureScheme::ECDSA_SHA1_Legacy,
            ]
        }
    }

    let mut tls_config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .dangerous()
    .with_custom_certificate_verifier(std::sync::Arc::new(AlwaysVerify))
    .with_no_client_auth();
    tls_config.alpn_protocols = vec![b"h3".to_vec()];

    let quic_tls_config =
        quinn_proto::crypto::rustls::QuicClientConfig::try_from(tls_config).unwrap();
    let mut quinn_client_config =
        quinn_proto::ClientConfig::new(std::sync::Arc::new(quic_tls_config));

    let mut transport_config = quinn_proto::TransportConfig::default();
    transport_config.max_idle_timeout(Some(std::time::Duration::from_secs(10).try_into().unwrap()));
    transport_config.keep_alive_interval(Some(std::time::Duration::from_millis(200)));
    quinn_client_config.transport_config(std::sync::Arc::new(transport_config));

    quinn_client_config
}
