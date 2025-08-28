# Nevy

Nevy is a quic networking library built with [quinn](https://github.com/quinn-rs/quinn) in bevy.

## Getting started

To start a connection with quic you need an endpoint.
An endpoint is an entity that contains the state machine for all of it's connections.

```rs
let endpoint_entity = commands
    .spawn(
        QuicEndpoint::new(
            "0.0.0.0:0", // Bind to any port.
            quinn_proto::EndpointConfig::default(),
            None, // No server config.
            AlwaysRejectIncoming::new(), // never accept incoming connections.
        )
        .unwrap(),
    )
    .id();
```

To open a connection you insert the `ConnectionOf` component along with a `ConnectionConfig`.
This will also insert a `QuicConnection` onto the entity if opening the connection succeeds,
and will always insert a `ConnectionStatus`.
This component is an identifier for the connection within the endpoint.

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

Nevy uses relations between `ConnectionOf` and `EndpointOf` to represent it's connections.
This means that to respond to incoming connections you can query for added connections.

```rs
connection_q: Query<Entity, Added<QuicConnection>>
```

To interact with a `QuicConnection` you need to retrieve it's state from the `QuicEndpoint` it is a `ConnectionOf`.

This is a system that will send a message on a quic stream whenever a connection establishes.

```rs
fn send_message(
    connection_q: Query<
        (&ConnectionOf, &QuicConnection, &ConnectionStatus),
        Changed<ConnectionStatus>,
    >,
    mut endpoint_q: Query<&mut QuicEndpoint>,
) -> Result<(), BevyError> {
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
        let stream_id = connection
            .open_stream(Dir::Uni)
            .ok_or("Streams should not be exhausted")?;

        // write some data
        connection.write_send_stream(stream_id, "Hello Server!".as_bytes())?;

        // finish the stream
        connection.finish_send_stream(stream_id)?;

        info!("Connection established, sent message");
    }

    Ok(())
}
```

Lastly, when connections close the entity is not despawned nor are any of it's connection components removed.
This is so that you can still retrieve the connection state from it's endpoint to read any remaining data.
Make sure to despawn any closed connections once you are done with them.

The entity can be reused for another connection once it's `ConnectionOf` component is removed or replaced.

## Bevy versions

| Bevy | Nevy |
| -----| ---- |
| 0.16 | 0.1  |
