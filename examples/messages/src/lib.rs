use bevy::prelude::*;
use nevy::prelude::*;
use serde::{Deserialize, Serialize};

pub fn build(app: &mut App) {
    // We use the empty type `()` as a marker for the "default" protocol.
    //
    // Third party crates can define their own messaging protocols which can be included in our protocol,
    // but this isn't seen here.
    app.init_protocol::<()>();

    // This assigns `HelloServer` an incremental id.
    // Because this build function is called on the client and the server,
    // they will both know the id of this message
    app.add_protocol_message::<(), HelloServer>();
}

#[derive(Serialize, Deserialize)]
pub struct HelloServer {
    pub data: String,
}
