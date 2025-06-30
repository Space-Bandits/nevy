use bevy::prelude::*;
use nevy::*;

fn main() {
    let mut app = App::new();

    app.add_plugins(MinimalPlugins);
    app.add_plugins(bevy::log::LogPlugin {
        level: bevy::log::tracing::Level::DEBUG,
        ..default()
    });
    app.add_plugins(NevyPlugin::default());

    app.add_systems(Startup, setup);
    app.add_systems(Update, (insert_streams, receive_messages));

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

#[derive(Component, Default)]
struct ConnectionStreams {
    streams: Vec<(StreamId, Vec<u8>)>,
}

fn insert_streams(mut commands: Commands, connection_q: Query<Entity, Added<QuicConnection>>) {
    for entity in connection_q.iter() {
        commands.entity(entity).insert(ConnectionStreams::default());
    }
}

fn receive_messages(
    mut connection_q: Query<(&mut ConnectionStreams, &ConnectionOf, &QuicConnection)>,
    mut endpoint_q: Query<&mut QuicEndpoint>,
) -> Result {
    for (mut streams, connection_of, connection) in connection_q.iter_mut() {
        let mut endpoint = endpoint_q.get_mut(**connection_of).unwrap();

        let Ok(connection) = endpoint.get_connection(connection) else {
            continue;
        };

        while let Some(stream_id) = connection.accept_stream(Direction::Uni) {
            streams.streams.push((stream_id, Vec::new()));
        }

        while let Some(event) = connection.poll_stream_event() {
            match dbg!(event) {
                _ => (),
            }
        }

        streams
            .streams
            .retain_mut(|&mut (stream_id, ref mut buffer)| loop {
                match connection.read_recv_stream(stream_id, usize::MAX, true) {
                    Ok(Some(Chunk { data, .. })) => buffer.extend(data),
                    Ok(None) => break false,
                    Err(StreamReadError::Blocked) => break true,
                    Err(err) => {
                        error!("stream encountered error: {}", err);
                        break false;
                    }
                }
            });
    }

    Ok(())
}

fn create_server_endpoint_config() -> nevy::quinn_proto::ServerConfig {
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
        nevy::quinn_proto::crypto::rustls::QuicServerConfig::try_from(tls_config).unwrap();

    let mut server_config =
        nevy::quinn_proto::ServerConfig::with_crypto(std::sync::Arc::new(quic_tls_config));

    let mut transport_config = nevy::quinn_proto::TransportConfig::default();
    transport_config.max_idle_timeout(Some(std::time::Duration::from_secs(10).try_into().unwrap()));
    transport_config.keep_alive_interval(Some(std::time::Duration::from_millis(200)));

    server_config.transport = std::sync::Arc::new(transport_config);

    server_config
}
