use std::{
    any::{Any, TypeId},
    collections::HashMap,
    hash::{Hash, Hasher},
    marker::PhantomData,
};

use bevy::prelude::*;

#[derive(Component)]
#[require(Protocol)]
struct ProtocolMarker<P>(PhantomData<P>);

#[derive(Component, Default)]
struct Protocol {
    messages: Vec<Box<dyn MessageId>>,
    lookup: HashMap<Box<dyn MessageId>, usize>,
}

trait MessageId: Send + Sync + 'static {
    fn type_id(&self) -> TypeId {
        TypeId::of::<Self>()
    }
}

impl<T> MessageId for PhantomData<T> where T: Send + Sync + 'static {}

impl Hash for Box<dyn MessageId> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.type_id().hash(state);
    }
}

impl PartialEq for Box<dyn MessageId> {
    fn eq(&self, other: &Self) -> bool {
        self.type_id() == other.type_id()
    }
}

pub trait ProtocolBuilder {
    fn init_protocol<P>(&mut self)
    where
        P: Send + Sync + 'static;
}

impl ProtocolBuilder for App {
    fn init_protocol<P>(&mut self)
    where
        P: Send + Sync + 'static,
    {
        self.world_mut().spawn(ProtocolMarker::<P>(PhantomData));
    }
}
