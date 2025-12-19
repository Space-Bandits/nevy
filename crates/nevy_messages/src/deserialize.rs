use std::{collections::VecDeque, marker::PhantomData};

use bevy::prelude::*;

#[derive(Component)]
pub struct ReceivedMessages<T, P = ()> {
    _p: PhantomData<P>,
    messages: VecDeque<T>,
}

impl<T, P> Default for ReceivedMessages<T, P> {
    fn default() -> Self {
        ReceivedMessages {
            _p: PhantomData,
            messages: VecDeque::new(),
        }
    }
}

#[derive(Component)]
pub struct AddReceivedMessages<P = ()> {
    _p: PhantomData<P>,
}

impl<P> Default for AddReceivedMessages<P> {
    fn default() -> Self {
        AddReceivedMessages { _p: PhantomData }
    }
}

pub(crate) fn build<P>(app: &mut App)
where
    P: Send + Sync + 'static,
{
}

pub(crate) fn build_message<T, P>(app: &mut App)
where
    T: Send + Sync + 'static,
    P: Send + Sync + 'static,
{
}
