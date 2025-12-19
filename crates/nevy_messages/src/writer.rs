use bevy::prelude::*;
use bytes::Bytes;
use nevy_transport::prelude::*;
use serde::Serialize;

use crate::{protocol::MessageId, varint::VarInt};

pub struct MessageStreamState {
    stream: Stream,
    buffer: Option<Bytes>,
}

impl MessageStreamState {
    pub fn new(mut connection: Connection) -> Result<Self> {
        let stream = connection.new_stream(StreamRequirements::RELIABLE_ORDERED)?;

        Ok(MessageStreamState {
            stream,
            buffer: None,
        })
    }

    pub fn flush(&mut self, mut connection: Connection) -> Result {
        let Some(bytes) = self.buffer.as_mut() else {
            return Ok(());
        };

        let written = connection.write(&self.stream, bytes.clone(), true)?;
        let _ = bytes.split_to(written);

        if bytes.is_empty() {
            self.buffer = None;
        }

        Ok(())
    }

    pub fn write<T, P>(
        &mut self,
        mut connection: Connection,
        message_id: MessageId<P>,
        message: &T,
    ) -> Result<bool>
    where
        T: Serialize,
    {
        self.flush(connection.reborrow())?;

        if self.buffer.is_some() {
            return Ok(false);
        }

        let message = bincode::serde::encode_to_vec(message, crate::bincode_config())?;

        let mut buffer = Vec::new();

        VarInt::from_u64(message_id.id as u64)
            .ok_or("Message id was too big for VarInt")?
            .encode(&mut buffer);

        VarInt::from_u64(message.len() as u64)
            .ok_or("Message length was too big for VarInt")?
            .encode(&mut buffer);

        buffer.extend(message);

        self.buffer = Some(buffer.into());

        self.flush(connection)?;

        Ok(true)
    }
}
