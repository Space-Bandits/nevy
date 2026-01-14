use bevy::prelude::*;
use nevy::prelude::*;
use serde::{Deserialize, Serialize};

pub fn build(app: &mut App) {
    app.init_protocol::<()>();

    app.add_protocol_message::<(), HelloServer>();
}

#[derive(Serialize, Deserialize)]
pub struct HelloServer {
    pub data: String,
}
