pub use crate::{
    Connection, ConnectionContext, ConnectionOf, ConnectionStatus, DEFAULT_TRANSPORT_SCHEDULE,
    Endpoint, EndpointOf, Stream, StreamReadError, StreamRequirements, Transport,
    TransportUpdateSystems,
};

#[cfg(feature = "quic")]
pub use crate::protocols::quic::{
    QuicTransportPlugin,
    connection::QuicConnectionContext,
    endpoint::{
        IncomingQuicConnection, QuicConnectionClosedReason, QuicConnectionConfig,
        QuicConnectionFailedReason, QuicEndpoint,
    },
    quinn_proto,
};

// Native WebTransport (desktop/server)
#[cfg(feature = "webtransport")]
pub use crate::protocols::webtransport::{
    WebTransportPlugin,
    connection::{WebTransportConnectionContext, WebTransportStreamId},
    endpoint::{
        IncomingWebTransportConnection, WebTransportConnectionClosedReason,
        WebTransportConnectionConfig, WebTransportConnectionFailedReason, WebTransportEndpoint,
    },
    SessionState,
};

// Browser WebTransport (WASM)
#[cfg(all(feature = "webtransport-web", target_arch = "wasm32"))]
pub use crate::protocols::webtransport_web::{
    WebTransportWebPlugin,
    connection::{WebTransportWebConnectionContext, WebTransportWebStreamId},
    endpoint::{WebTransportWebConfig, WebTransportWebEndpoint},
};
