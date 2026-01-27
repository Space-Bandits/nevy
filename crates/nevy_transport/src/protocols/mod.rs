#[cfg(feature = "quic")]
pub mod quic;

// Native WebTransport (H3/QUIC)
#[cfg(feature = "webtransport")]
pub mod webtransport;

// Browser WebTransport (web_sys)
#[cfg(all(feature = "webtransport-web", target_arch = "wasm32"))]
pub mod webtransport_web;
