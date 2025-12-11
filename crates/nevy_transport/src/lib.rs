use bevy::prelude::*;

pub mod prelude;
pub mod protocols;

#[derive(Component, Deref, DerefMut)]
pub struct Endpoint(Box<dyn Transport>);

pub trait Transport: Send + Sync {
    fn get_connection<'a>(&'a mut self, connection: Entity) -> Option<ConnectionMut<'a>>;
}

#[derive(Deref, DerefMut)]
pub struct ConnectionMut<'a>(Box<dyn Connection + 'a>);

pub trait Connection {}
