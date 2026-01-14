# Nevy

Nevy is a networking library for the [bevy](https://bevyengine.org) game engine.
It provides a type erased api over substitutable networking layers.

This is so that your games can use different protocols like QUIC or Web Transport,
either at the same time if they are a server, or by choosing a different one depending
on what target they are compiled for.

Currently nevy only supports QUIC using [quinn](https://github.com/quinn-rs/quinn), but we will add support for more as we need them.

## Getting started

First, insert the plugins.

```rs
use nevy::prelude::*;

app.add_plugins(QuicTransportPlugin::default());
```

An endpoint is an entity that holds an `Endpoint` component.
How you create an endpoint depends on the protocol you are using.

Here, `QuicEndpoint::new(...)` returns an `Endpoint` component which you can insert onto an entity.

```rs
let endpoint_entity = commands
    .spawn(
        QuicEndpoint::new(
            "0.0.0.0:0",
            quinn_proto::EndpointConfig::default(),
            None,
        ).unwrap(),
    )
    .id();
```

Connections are represented by an entity that is related to its endpoint by a `ConnectionOf` component.
This component is only ever inserted by you, by either opening or accepting a connection.

Different protocols will expect supporting components on the same entity to start a connection.
In this case a `QuicConnectionConfig` is needed to open a connection.

```rs
commands.spawn((
    ConnectionOf(endpoint_entity),
    QuicConnectionConfig {
        client_config: create_connection_config(),
        address: "127.0.0.1:27518".parse().unwrap(),
        server_name: "example.server".to_string(),
    },
));
```

Whenever a `ConnectionOf` component exists, a `Connection` state can be retrieved from its associated `Endpoint`.
This is how you perform operations like sending and receiving data, closing the connection, etc.
Any connection entity will also have a `ConnectionStatus` component, which can be used to check if the connection is connecting, established, closed or failed.

```rs
/// When the connection establishes, opens a stream, writes a message, and finishes the stream.
fn write_messages(
    connection_q: Query<(Entity, &ConnectionOf, &ConnectionStatus), Changed<ConnectionStatus>>,
    mut endpoint_q: Query<&mut Endpoint>,
) -> Result {
    for (connection_entity, &ConnectionOf(endpoint_entity), status) in &connection_q {
        let ConnectionStatus::Established = status else {
            continue;
        };

        let mut endpoint = endpoint_q.get_mut(endpoint_entity)?;
        let mut connection = endpoint.get_connection(connection_entity)?;

        let stream = connection.new_stream(StreamRequirements::UNRELIABLE)?;

        let written = connection.write(&stream, "Hello server!".as_bytes().into(), false)?;
        info!("Wrote {} bytes", written);

        connection.close_send_stream(&stream, true)?;
    }

    Ok(())
}
```

See the examples for more details, including how nevy's messaging crate works for sending structured data instead of just byte streams.

## Bevy versions

| Bevy | Nevy |
| -----| ---- |
| 0.16 | 0.1  |
| 0.17 | 0.3  |
