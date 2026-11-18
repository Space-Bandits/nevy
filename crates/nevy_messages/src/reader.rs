use std::collections::VecDeque;

use bevy::{platform::collections::HashMap, prelude::*};
use bytes::Bytes;
use nevy_transport::{prelude::*, varint::*};

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

impl Default for MessageStreamReaderState {
    fn default() -> Self {
        MessageStreamReaderState::ReceivingId {
            decoder: VarIntDecoder::default(),
        }
    }
}

pub(crate) fn accept_streams(
    mut connection_q: Query<(Entity, &ConnectionOf, &mut MessageStreamReaders)>,
    mut endpoint_q: Query<&mut Endpoint>,
) -> Result {
    for (connection_entity, &ConnectionOf(endpoint_entity), mut readers) in &mut connection_q {
        let mut endpoint = endpoint_q.get_mut(endpoint_entity)?;
        let mut connection = endpoint.get_connection(connection_entity)?;

        while let Some((stream, _)) = connection.accept_stream() {
            readers
                .readers
                .push((stream, MessageStreamReaderState::default()));
        }
    }

    Ok(())
}

/// Takes message streams from [`HeaderedStreams`] instead of accepting them directly.
#[cfg(feature = "stream_headers")]
pub(crate) fn accept_headered_streams(
    mut connection_q: Query<(&mut HeaderedStreams, &mut MessageStreamReaders)>,
    header: Res<crate::writer::MessageStreamHeader>,
) {
    for (mut streams, mut readers) in &mut connection_q {
        let readers = readers.as_mut();

        while let Some(HeaderedStream {
            stream, leftover, ..
        }) = streams.take(**header)
        {
            let mut state = MessageStreamReaderState::default();

            if !leftover.is_empty() {
                consume_chunk(&mut state, leftover, &mut readers.buffers);
            }

            readers.readers.push((stream, state));
        }
    }
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
                let chunk = match connection.read(stream)? {
                    Err(StreamReadError::Closed) => break false,
                    Err(StreamReadError::Blocked) => break true,
                    Ok(chunk) => chunk,
                };

                consume_chunk(reader, chunk, &mut readers.buffers);
            });
        }

        let mut keep = keep.into_iter();
        readers.readers.retain(|_| keep.next().unwrap_or_default());
    }

    Ok(())
}

/// Feeds a chunk of stream data through a reader, pushing every completed message to its buffer.
fn consume_chunk(
    reader: &mut MessageStreamReaderState,
    mut chunk: Bytes,
    buffers: &mut HashMap<usize, VecDeque<Box<[u8]>>>,
) {
    loop {
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

                    // Skip chunk empty check if message is length zero.
                    if length.as_u64() == 0 {
                        continue;
                    }
                }
            }
            &mut MessageStreamReaderState::ReceivingMessage {
                id,
                length,
                ref mut buffer,
            } => {
                // take data from chunk
                let remaining_bytes = length - buffer.len();
                buffer.extend(chunk.split_to(remaining_bytes.min(chunk.len())).as_ref());

                // if we received the whole message update the state and push the message to its sorted buffer
                if buffer.len() == length {
                    let message = std::mem::take(buffer).into_boxed_slice();

                    *reader = MessageStreamReaderState::default();

                    buffers.entry(id).or_default().push_back(message);
                }
            }
        }

        // Consume bits of the chunk until it is empty.
        //
        // Check is at the end of the loop to allow `continue` to skip check in case of messange length zero.
        if chunk.is_empty() {
            break;
        }
    }
}
