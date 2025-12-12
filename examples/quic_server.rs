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
    app.add_plugins(QuicTransportPlugin::default());

    app.add_systems(Startup, setup);
    app.add_observer(accept_connections);
    app.add_observer(log_status_changes);

    app.run();
}

fn setup(mut commands: Commands) {
    commands.spawn(
        QuicEndpoint::new(
            "0.0.0.0:27518",
            quinn_proto::EndpointConfig::default(),
            Some(create_server_endpoint_config()),
        )
        .unwrap(),
    );
}

fn accept_connections(
    insert: On<Insert, IncomingQuicConnection>,
    mut commands: Commands,
    connection_q: Query<&IncomingQuicConnection>,
) -> Result {
    let &IncomingQuicConnection {
        endpoint_entity, ..
    } = connection_q.get(insert.entity)?;

    commands
        .entity(insert.entity)
        .insert(ConnectionOf(endpoint_entity));

    Ok(())
}

fn log_status_changes(
    insert: On<Insert, ConnectionStatus>,
    status_q: Query<&ConnectionStatus>,
) -> Result {
    let status = status_q.get(insert.entity)?;
    info!("Connection status changed to {:?}", status);
    Ok(())
}

fn create_server_endpoint_config() -> quinn_proto::ServerConfig {
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
        quinn_proto::crypto::rustls::QuicServerConfig::try_from(tls_config).unwrap();

    let mut server_config =
        quinn_proto::ServerConfig::with_crypto(std::sync::Arc::new(quic_tls_config));

    let mut transport_config = quinn_proto::TransportConfig::default();
    transport_config.max_idle_timeout(Some(std::time::Duration::from_secs(10).try_into().unwrap()));
    transport_config.keep_alive_interval(Some(std::time::Duration::from_millis(200)));

    server_config.transport = std::sync::Arc::new(transport_config);

    server_config
}
