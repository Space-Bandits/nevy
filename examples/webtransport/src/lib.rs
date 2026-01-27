//! WASM WebTransport client example.
//!
//! Build with:
//!   cargo build --target wasm32-unknown-unknown --features web --no-default-features
//!   wasm-bindgen --out-dir pkg --target web target/wasm32-unknown-unknown/debug/webtransport_example.wasm

#[cfg(all(target_arch = "wasm32", feature = "web"))]
mod wasm_client {
    use bevy::{
        log::LogPlugin,
        prelude::*,
    };
    use nevy::prelude::*;
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen(start)]
    pub fn start() {
        console_error_panic_hook::set_once();
        web_sys::console::log_1(&"WASM WebTransport client loaded.".into());
    }

    #[wasm_bindgen]
    pub fn run_game(url: String, cert_hash_base64: String) {
        web_sys::console::log_1(&format!("Connecting to: {}", url).into());

        // Decode the base64 certificate hash
        let cert_hash = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            &cert_hash_base64,
        )
        .expect("Invalid base64 certificate hash");

        let mut app = App::new();

        app.add_plugins(MinimalPlugins);
        app.add_plugins(LogPlugin::default());
        app.add_plugins(WebTransportWebPlugin::default());

        app.insert_resource(ConnectionConfig {
            url,
            cert_hash,
            connected: false,
        });

        app.add_systems(Startup, setup);
        app.add_observer(on_status_change);
        app.add_systems(Update, (check_connection, read_messages, send_periodic_message));

        app.run();
    }

    #[derive(Resource)]
    struct ConnectionConfig {
        url: String,
        cert_hash: Vec<u8>,
        connected: bool,
    }

    #[derive(Resource)]
    struct GameConnection {
        endpoint: Entity,
        connection: Entity,
        stream: Option<nevy::prelude::Stream>,
        message_timer: f32,
    }

    fn setup(mut commands: Commands, config: Res<ConnectionConfig>) {
        web_sys::console::log_1(&"Setting up WebTransport connection...".into());

        // Create endpoint (browser-based, no server config needed)
        let endpoint = commands.spawn(WebTransportWebEndpoint::new()).id();

        // Create connection with certificate hash for self-signed cert
        let connection = commands
            .spawn((
                WebTransportWebConfig {
                    url: config.url.clone(),
                    server_certificate_hashes: Some(vec![config.cert_hash.clone()]),
                },
                ConnectionOf(endpoint),
            ))
            .id();

        commands.insert_resource(GameConnection {
            endpoint,
            connection,
            stream: None,
            message_timer: 0.0,
        });
    }

    fn on_status_change(
        trigger: On<Insert, ConnectionStatus>,
        status_q: Query<&ConnectionStatus>,
        mut config: ResMut<ConnectionConfig>,
    ) -> Result {
        let status = status_q.get(trigger.entity)?;
        web_sys::console::log_1(&format!("Connection status: {:?}", status).into());

        if matches!(status, ConnectionStatus::Established) {
            config.connected = true;
            web_sys::console::log_1(&"Connected! Opening stream...".into());
        }

        Ok(())
    }

    fn check_connection(
        mut game: ResMut<GameConnection>,
        mut endpoint_q: Query<&mut Endpoint>,
        config: Res<ConnectionConfig>,
    ) {
        if !config.connected || game.stream.is_some() {
            return;
        }

        let Ok(mut endpoint) = endpoint_q.get_mut(game.endpoint) else {
            return;
        };

        let Ok(mut connection) = endpoint.get_connection(game.connection) else {
            return;
        };

        match connection.new_stream(StreamRequirements::RELIABLE_ORDERED.with_bidirectional(true)) {
            Ok(stream) => {
                web_sys::console::log_1(&"Bidirectional stream opened!".into());
                game.stream = Some(stream);
            }
            Err(e) => {
                web_sys::console::log_1(&format!("Failed to open stream: {:?}", e).into());
            }
        }
    }

    fn send_periodic_message(
        mut game: ResMut<GameConnection>,
        mut endpoint_q: Query<&mut Endpoint>,
        time: Res<Time>,
        config: Res<ConnectionConfig>,
    ) {
        if !config.connected {
            return;
        }

        game.message_timer += time.delta_secs();

        if game.message_timer < 2.0 {
            return;
        }
        game.message_timer = 0.0;

        let Some(ref stream) = game.stream else {
            return;
        };

        let Ok(mut endpoint) = endpoint_q.get_mut(game.endpoint) else {
            return;
        };

        let Ok(mut connection) = endpoint.get_connection(game.connection) else {
            return;
        };

        let message = format!("Hello from WASM! t={:.1}s", time.elapsed_secs());
        match connection.write(stream, bytes::Bytes::from(message.clone()), false) {
            Ok(n) if n > 0 => {
                web_sys::console::log_1(&format!("Sent: {}", message).into());
            }
            Ok(_) => {}
            Err(e) => {
                web_sys::console::log_1(&format!("Write error: {:?}", e).into());
            }
        }
    }

    fn read_messages(
        game: Res<GameConnection>,
        mut endpoint_q: Query<&mut Endpoint>,
        config: Res<ConnectionConfig>,
    ) {
        if !config.connected {
            return;
        }

        let Some(ref stream) = game.stream else {
            return;
        };

        let Ok(mut endpoint) = endpoint_q.get_mut(game.endpoint) else {
            return;
        };

        let Ok(mut connection) = endpoint.get_connection(game.connection) else {
            return;
        };

        match connection.read(stream) {
            Ok(Ok(data)) => {
                let text = String::from_utf8_lossy(&data);
                web_sys::console::log_1(&format!("Received: {}", text).into());
            }
            Ok(Err(StreamReadError::Blocked)) => {}
            Ok(Err(StreamReadError::Closed)) => {
                web_sys::console::log_1(&"Stream closed".into());
            }
            Err(e) => {
                web_sys::console::log_1(&format!("Read error: {:?}", e).into());
            }
        }
    }
}

#[cfg(all(target_arch = "wasm32", feature = "web"))]
pub use wasm_client::*;
