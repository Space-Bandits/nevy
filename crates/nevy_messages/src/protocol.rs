use std::marker::PhantomData;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Resource, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MessageId<P = ()> {
    _p: PhantomData<P>,
    id: usize,
}

/// Stores how many different messages are in a particular protocol.
#[derive(Resource)]
pub(crate) struct MessageIdCount<P> {
    _p: PhantomData<P>,
    pub count: usize,
}

pub struct MessageProtocol<I, P = ()> {
    _p: PhantomData<P>,
    /// A sorted list of messages
    messages: Vec<(I, Box<dyn BuildMessage<P>>)>,
}

trait BuildMessage<P> {
    fn build_deserialize(&self, app: &mut App);
}

impl<T, P> BuildMessage<P> for PhantomData<T>
where
    T: Send + Sync + 'static,
    P: Send + Sync + 'static,
{
    fn build_deserialize(&self, app: &mut App) {
        crate::deserialize::build_message::<T, P>(app);
    }
}

impl<I, P> MessageProtocol<I, P> {
    pub fn add_message<T>(&mut self, id: I)
    where
        I: Ord,
        T: Send + Sync + 'static,
        P: Send + Sync + 'static,
    {
        let (Ok(index) | Err(index)) = self.messages.binary_search_by(|(probe, _)| probe.cmp(&id));

        self.messages
            .insert(index, (id, Box::new(PhantomData::<T>)));
    }

    pub fn add_to_app(&self, app: &mut App)
    where
        P: Send + Sync + 'static,
    {
        let mut id = 0;

        for (_, builder) in &self.messages {
            app.insert_resource(MessageId::<P> {
                _p: PhantomData,
                id,
            });

            id += 1;

            builder.build_deserialize(app);
        }

        app.insert_resource(MessageIdCount::<P> {
            _p: PhantomData,
            count: id,
        });

        crate::deserialize::build::<P>(app);
    }
}

pub trait AddMessageProtocol<P = ()> {
    fn add_message_protocol<I>(&mut self, protocol: &MessageProtocol<I>) -> &mut Self;
}

impl<P> AddMessageProtocol<P> for App
where
    P: Send + Sync + 'static,
{
    fn add_message_protocol<I>(&mut self, protocol: &MessageProtocol<I>) -> &mut Self {
        protocol.add_to_app(self);

        self
    }
}
