use crate::{Connection, ConnectionMut, Transport};
use bevy::{platform::collections::HashMap, prelude::*};

pub struct QuicEndpoint {
    connections: HashMap<Entity, QuicConnectionState>,
}

pub struct QuicConnectionState {}

impl Transport for QuicEndpoint {
    fn get_connection<'a>(&'a mut self, connection: Entity) -> Option<ConnectionMut<'a>> {
        let state = self.connections.get_mut(&connection)?;

        Some(ConnectionMut(Box::new(QuicConnectionMut { state })))
    }
}

struct QuicConnectionMut<'a> {
    state: &'a mut QuicConnectionState,
}

impl<'a> Connection for QuicConnectionMut<'a> {}
