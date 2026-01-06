use nevy::prelude::*;
use serde::{Deserialize, Serialize};

pub fn message_protocol() -> MessageProtocol<usize> {
    ordered_protocol! {
        HelloServer,
    }
}

#[derive(Serialize, Deserialize)]
pub struct HelloServer {
    pub data: String,
}
