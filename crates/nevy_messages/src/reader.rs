use std::collections::VecDeque;

use crate::varint::*;
use bevy::{platform::collections::HashMap, prelude::*};
use nevy_transport::prelude::*;

#[derive(Component, Default)]
pub struct MessageStreamReaders {
    readers: Vec<(Stream, MessageStreamReaderState)>,
    pub(crate) buffers: HashMap<usize, VecDeque<Box<[u8]>>>,
}

enum MessageStreamReaderState {
    ReceivingId {
        decoder: VarIntDecoder,
    },
    ReceivingLength {
        id: usize,
        decoder: VarIntDecoder,
    },
    ReceivingMessage {
        id: usize,
        length: usize,
        buffer: Vec<u8>,
    },
}

pub(crate) fn accept_streams(
    mut connection_q: Query<(Entity, &ConnectionOf, &mut MessageStreamReaders)>,
    mut endpoint_q: Query<&mut Endpoint>,
) -> Result {
    for (connection_entity, &ConnectionOf(endpoint_entity), mut readers) in &mut connection_q {
        let mut endpoint = endpoint_q.get_mut(endpoint_entity)?;
        let mut connection = endpoint.get_connection(connection_entity)?;

        while let Some((stream, _)) = connection.accept_stream() {
            readers.readers.push((
                stream,
                MessageStreamReaderState::ReceivingId {
                    decoder: VarIntDecoder::default(),
                },
            ));
        }
    }

    Ok(())
}

pub(crate) fn read_streams(
    mut connection_q: Query<(Entity, &ConnectionOf, &mut MessageStreamReaders)>,
    mut endpoint_q: Query<&mut Endpoint>,
) -> Result {
    for (connection_entity, &ConnectionOf(endpoint_entity), mut readers) in &mut connection_q {
        let mut endpoint = endpoint_q.get_mut(endpoint_entity)?;

        let mut connection = endpoint.get_connection(connection_entity)?;

        let mut keep = Vec::with_capacity(readers.readers.len());

        let readers = readers.as_mut();
        for (stream, reader) in &mut readers.readers {
            keep.push(loop {
                let mut chunk = match connection.read(stream)? {
                    Err(StreamReadError::Closed) => break false,
                    Err(StreamReadError::Blocked) => break true,
                    Ok(chunk) => chunk,
                };

                loop {
                    // Consume bits of the chunk until it is empty.
                    if chunk.is_empty() {
                        break;
                    }

                    match reader {
                        &mut MessageStreamReaderState::ReceivingId { ref mut decoder } => {
                            let (result, used) = decoder.push_bytes(chunk.as_ref());
                            chunk = chunk.slice(used..);

                            if let Some(id) = result {
                                *reader = MessageStreamReaderState::ReceivingLength {
                                    id: id.as_u64() as usize,
                                    decoder: VarIntDecoder::default(),
                                };
                            }
                        }
                        &mut MessageStreamReaderState::ReceivingLength {
                            id,
                            ref mut decoder,
                        } => {
                            let (result, used) = decoder.push_bytes(chunk.as_ref());
                            chunk = chunk.slice(used..);

                            if let Some(length) = result {
                                *reader = MessageStreamReaderState::ReceivingMessage {
                                    id,
                                    length: length.as_u64() as usize,
                                    buffer: Vec::new(),
                                };
                            }
                        }
                        &mut MessageStreamReaderState::ReceivingMessage {
                            id,
                            length,
                            ref mut buffer,
                        } => {
                            // take data from chunk
                            let remaining_bytes = length - buffer.len();
                            buffer
                                .extend(chunk.split_to(remaining_bytes.min(chunk.len())).as_ref());

                            // if we received the whole message update the state and push the message to its sorted buffer
                            if buffer.len() == length {
                                let message = std::mem::take(buffer).into_boxed_slice();

                                *reader = MessageStreamReaderState::ReceivingId {
                                    decoder: VarIntDecoder::default(),
                                };

                                readers.buffers.entry(id).or_default().push_back(message);
                            }
                        }
                    }
                }
            });
        }

        let mut keep = keep.into_iter();
        readers.readers.retain(|_| keep.next().unwrap_or_default());
    }

    Ok(())
}
