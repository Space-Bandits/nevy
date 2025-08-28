use std::sync::Arc;

use bevy::prelude::*;
use nevy::*;

fn main() {
    let mut app = App::new();

    app.add_plugins(MinimalPlugins);
    app.add_plugins(bevy::log::LogPlugin::default());
    app.add_plugins(NevyPlugin::default());

    app.add_systems(Startup, setup);
    app.add_systems(Update, (send_message, close_connection, close_app).chain());

    app.run();
}

fn setup(mut commands: Commands) {
    let endpoint_entity = commands
        .spawn(
            QuicEndpoint::new(
                "0.0.0.0:0",
                quinn_proto::EndpointConfig::default(),
                None,
                AlwaysRejectIncoming::new(),
            )
            .unwrap(),
        )
        .id();

    commands.spawn((
        // Inserting this component will open the connection.
        ConnectionOf(endpoint_entity),
        // Opening a connection will fail if this component does not exist on the same entity.
        QuicConnectionConfig {
            client_config: create_connection_config(),
            address: "127.0.0.1:27518".parse().unwrap(),
            server_name: "example.server".to_string(),
        },
    ));
}

fn send_message(
    connection_q: Query<
        (&ConnectionOf, &QuicConnection, &ConnectionStatus),
        Changed<ConnectionStatus>,
    >,
    mut endpoint_q: Query<&mut QuicEndpoint>,
) -> Result {
    for (connection_of, connection, status) in connection_q.iter() {
        // only operate on connections that have just changed to established
        let ConnectionStatus::Established = status else {
            continue;
        };

        // get the endpoint component
        let mut endpoint = endpoint_q.get_mut(**connection_of)?;

        // get the connection state from the endpoint
        let connection = endpoint.get_connection(connection)?;

        // open a unidirectional stream on the connection.
        let stream_id = connection.open_stream(Dir::Uni)?;

        // write some data
        connection.write_send_stream(stream_id, "Hello Server!".as_bytes())?;

        // finish the stream
        connection.finish_send_stream(stream_id)?;

        info!("Connection established, sent message");
    }

    Ok(())
}

fn close_connection(
    connection_q: Query<(&ConnectionOf, &QuicConnection, &ConnectionStatus)>,
    mut endpoint_q: Query<&mut QuicEndpoint>,
) -> Result {
    for (connection_of, connection, status) in connection_q.iter() {
        let ConnectionStatus::Established = status else {
            continue;
        };

        let mut endpoint = endpoint_q.get_mut(**connection_of)?;

        let connection = endpoint.get_connection(connection)?;

        if connection.get_open_send_streams() == 0 {
            connection.close(0, default())?;

            info!("all data sent, closing connection")
        }
    }

    Ok(())
}

fn close_app(
    connection_q: Query<&ConnectionStatus, Changed<ConnectionStatus>>,
    mut app_exit_w: EventWriter<AppExit>,
) {
    for status in &connection_q {
        let (ConnectionStatus::Closed { .. } | ConnectionStatus::Failed { .. }) = status else {
            continue;
        };

        app_exit_w.write_default();
    }
}

/// creates the quinn client config for a connection with a server
///
/// includes tls and transport config
fn create_connection_config() -> nevy::quinn_proto::ClientConfig {
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
    .with_custom_certificate_verifier(Arc::new(AlwaysVerify))
    .with_no_client_auth();
    tls_config.alpn_protocols = vec![b"h3".to_vec()];

    let quic_tls_config =
        nevy::quinn_proto::crypto::rustls::QuicClientConfig::try_from(tls_config).unwrap();
    let mut quinn_client_config =
        nevy::quinn_proto::ClientConfig::new(std::sync::Arc::new(quic_tls_config));

    let mut transport_config = nevy::quinn_proto::TransportConfig::default();
    transport_config.max_idle_timeout(Some(std::time::Duration::from_secs(10).try_into().unwrap()));
    transport_config.keep_alive_interval(Some(std::time::Duration::from_millis(200)));
    quinn_client_config.transport_config(std::sync::Arc::new(transport_config));

    quinn_client_config
}
