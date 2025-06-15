use bevy::prelude::*;
use nevy_quic::*;

fn main() {
    let mut app = App::new();

    app.add_plugins(MinimalPlugins);
    app.add_plugins(bevy::log::LogPlugin {
        level: bevy::log::tracing::Level::DEBUG,
        ..default()
    });
    app.add_plugins(NevyQuic::default());

    app.add_systems(Startup, setup);

    app.run();
}

fn setup(mut commands: Commands) {
    commands.spawn(
        QuicEndpoint::new(
            "0.0.0.0:27518",
            None,
            Some(create_server_endpoint_config()),
            AlwaysAcceptIncoming::new(),
        )
        .unwrap(),
    );
}

fn create_server_endpoint_config() -> nevy_quic::quinn_proto::ServerConfig {
    let cert = rcgen::generate_simple_self_signed(vec!["dev.nevy".to_string()]).unwrap();
    let key = rustls::pki_types::PrivateKeyDer::try_from(cert.key_pair.serialize_der()).unwrap();
    let chain = cert.cert.der().clone();

    let mut tls_config = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![chain], key)
    .unwrap();

    tls_config.max_early_data_size = u32::MAX;
    tls_config.alpn_protocols = vec![b"h3".to_vec()]; // this one is important

    let quic_tls_config =
        nevy_quic::quinn_proto::crypto::rustls::QuicServerConfig::try_from(tls_config).unwrap();

    let mut server_config =
        nevy_quic::quinn_proto::ServerConfig::with_crypto(std::sync::Arc::new(quic_tls_config));

    let mut transport_config = nevy_quic::quinn_proto::TransportConfig::default();
    transport_config.max_idle_timeout(Some(std::time::Duration::from_secs(10).try_into().unwrap()));
    transport_config.keep_alive_interval(Some(std::time::Duration::from_millis(200)));

    server_config.transport = std::sync::Arc::new(transport_config);

    server_config
}
