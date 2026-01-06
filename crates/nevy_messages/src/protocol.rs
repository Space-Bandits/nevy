use std::marker::PhantomData;

use bevy::prelude::*;
use serde::de::DeserializeOwned;

#[derive(Resource, Debug, PartialEq, Eq, Hash)]
pub struct MessageId<T, P = ()> {
    _p: PhantomData<(T, P)>,
    pub(crate) id: usize,
}

impl<T, P> Clone for MessageId<T, P> {
    fn clone(&self) -> Self {
        MessageId {
            _p: PhantomData,
            id: self.id,
        }
    }
}

impl<T, P> Copy for MessageId<T, P> {}

pub struct MessageProtocol<I, P = ()> {
    /// A sorted list of messages
    messages: Vec<(I, Box<dyn BuildMessage<P>>)>,
}

trait BuildMessage<P> {
    fn build(&self, app: &mut App, id: usize);
}

impl<T, P> BuildMessage<P> for PhantomData<T>
where
    T: Send + Sync + 'static + DeserializeOwned,
    P: Send + Sync + 'static,
{
    fn build(&self, app: &mut App, id: usize) {
        app.insert_resource(MessageId::<T> {
            _p: PhantomData,
            id,
        });

        crate::deserialize::build_message::<T, P>(app);
    }
}

impl<I, P> MessageProtocol<I, P> {
    pub fn new() -> Self {
        MessageProtocol {
            messages: Vec::new(),
        }
    }

    pub fn add_message<T>(&mut self, id: I)
    where
        I: Ord,
        T: Send + Sync + 'static + DeserializeOwned,
        P: Send + Sync + 'static,
    {
        let (Ok(index) | Err(index)) = self.messages.binary_search_by(|(probe, _)| probe.cmp(&id));

        self.messages
            .insert(index, (id, Box::new(PhantomData::<T>)));
    }

    pub fn with_message<T>(mut self, id: I) -> Self
    where
        I: Ord,
        T: Send + Sync + 'static + DeserializeOwned,
        P: Send + Sync + 'static,
    {
        self.add_message::<T>(id);
        self
    }

    pub fn add_to_app(&self, app: &mut App)
    where
        P: Send + Sync + 'static,
    {
        for (id, (_, builder)) in self.messages.iter().enumerate() {
            builder.build(app, id);
        }

        crate::deserialize::build::<P>(app);
    }
}

pub trait AddMessageProtocol {
    fn add_message_protocol<I, P>(&mut self, protocol: MessageProtocol<I, P>) -> &mut Self
    where
        P: Send + Sync + 'static;
}

impl AddMessageProtocol for App {
    fn add_message_protocol<I, P>(&mut self, protocol: MessageProtocol<I, P>) -> &mut Self
    where
        P: Send + Sync + 'static,
    {
        protocol.add_to_app(self);

        self
    }
}

#[macro_export]
macro_rules! ordered_protocol {
    (
        marker = $marker:ty,
        $($message:ty),*$(,)?
    ) => {
        {
            let mut id = 0;

            MessageProtocol::<usize, $marker>::new()

            $(
                .with_message::<$message>({
                    id += 1;
                    id
                })
            )*
        }
    };

    (
        $($message:ty),*$(,)?
    ) => {
        ordered_protocol!(
            marker = (),
            $($message),*
        )
    };
}
