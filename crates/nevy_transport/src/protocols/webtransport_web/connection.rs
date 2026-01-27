//! Browser WebTransport connection context implementation.

use std::collections::{HashMap, VecDeque};

use bevy::prelude::*;
use bytes::Bytes;
use js_sys::Uint8Array;
use wasm_bindgen::JsCast;
use web_sys::{
    ReadableStreamDefaultReader, WebTransport, WebTransportBidirectionalStream,
    WebTransportReceiveStream, WebTransportSendStream, WritableStreamDefaultWriter,
};

use crate::{
    Connection, ConnectionContext, MismatchedStreamError, Stream, StreamId, StreamReadError,
    StreamRequirements, UnsupportedStreamRequirementsError,
};

use super::promise::{PendingPromise, PendingRead};

/// Browser WebTransport stream identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WebTransportWebStreamId {
    /// Stream for sending/receiving datagrams.
    Datagrams,
    /// A bidirectional stream.
    Bidirectional(u32),
    /// A unidirectional send stream.
    UnidirectionalSend(u32),
    /// A unidirectional receive stream.
    UnidirectionalRecv(u32),
    /// A received datagram.
    ReceivedDatagram(usize),
}

impl StreamId for WebTransportWebStreamId {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn clone(&self) -> Stream {
        Stream::new(<Self as Clone>::clone(self))
    }

    fn eq(&self, rhs: &Stream) -> bool {
        match rhs.as_stream::<Self>() {
            Ok(other) => self == other,
            Err(_) => false,
        }
    }
}

/// State for a bidirectional stream.
pub(crate) struct BidiStreamState {
    pub send_writer: WritableStreamDefaultWriter,
    pub recv_reader: ReadableStreamDefaultReader,
    pub pending_read: Option<PendingRead>,
    pub read_buffer: VecDeque<Vec<u8>>,
}

/// State for a unidirectional send stream.
pub(crate) struct UniSendStreamState {
    pub writer: WritableStreamDefaultWriter,
}

/// State for a unidirectional receive stream.
pub(crate) struct UniRecvStreamState {
    pub reader: ReadableStreamDefaultReader,
    pub pending_read: Option<PendingRead>,
    pub read_buffer: VecDeque<Vec<u8>>,
}

/// Connected state for a WebTransport connection.
pub(crate) struct ConnectedState {
    pub transport: WebTransport,
    pub next_stream_id: u32,
    pub bidi_streams: HashMap<u32, BidiStreamState>,
    pub uni_send_streams: HashMap<u32, UniSendStreamState>,
    pub uni_recv_streams: HashMap<u32, UniRecvStreamState>,
    pub pending_bidi_open: HashMap<u32, PendingPromise<WebTransportBidirectionalStream>>,
    pub pending_uni_open: HashMap<u32, PendingPromise<WebTransportSendStream>>,
    pub incoming_bidi_reader: Option<ReadableStreamDefaultReader>,
    pub incoming_uni_reader: Option<ReadableStreamDefaultReader>,
    pub datagrams_reader: Option<ReadableStreamDefaultReader>,
    pub datagrams_writer: Option<WritableStreamDefaultWriter>,
    pub received_datagrams: VecDeque<Vec<u8>>,
    pub datagram_queue_offset: usize,
}

impl ConnectedState {
    pub fn new(transport: WebTransport) -> Self {
        // Set up incoming stream readers
        let incoming_bidi_reader = transport
            .incoming_bidirectional_streams()
            .get_reader()
            .dyn_into::<ReadableStreamDefaultReader>()
            .ok();

        let incoming_uni_reader = transport
            .incoming_unidirectional_streams()
            .get_reader()
            .dyn_into::<ReadableStreamDefaultReader>()
            .ok();

        // Set up datagram reader/writer
        let datagrams = transport.datagrams();
        let datagrams_reader = datagrams
            .readable()
            .get_reader()
            .dyn_into::<ReadableStreamDefaultReader>()
            .ok();
        let datagrams_writer = datagrams
            .writable()
            .get_writer()
            .ok();

        Self {
            transport,
            next_stream_id: 0,
            bidi_streams: HashMap::new(),
            uni_send_streams: HashMap::new(),
            uni_recv_streams: HashMap::new(),
            pending_bidi_open: HashMap::new(),
            pending_uni_open: HashMap::new(),
            incoming_bidi_reader,
            incoming_uni_reader,
            datagrams_reader,
            datagrams_writer,
            received_datagrams: VecDeque::new(),
            datagram_queue_offset: 0,
        }
    }

    pub fn poll_incoming(&mut self) {
        // Poll pending stream opens
        let bidi_ids: Vec<_> = self.pending_bidi_open.keys().copied().collect();
        for id in bidi_ids {
            if let Some(pending) = self.pending_bidi_open.get(&id) {
                if let Some(result) = pending.poll() {
                    self.pending_bidi_open.remove(&id);
                    if let Ok(stream) = result {
                        let send_writer = stream.writable().get_writer().ok();
                        let recv_reader = stream
                            .readable()
                            .get_reader()
                            .dyn_into::<ReadableStreamDefaultReader>()
                            .ok();

                        if let (Some(send_writer), Some(recv_reader)) = (send_writer, recv_reader) {
                            self.bidi_streams.insert(
                                id,
                                BidiStreamState {
                                    send_writer,
                                    recv_reader,
                                    pending_read: None,
                                    read_buffer: VecDeque::new(),
                                },
                            );
                        }
                    }
                }
            }
        }

        let uni_ids: Vec<_> = self.pending_uni_open.keys().copied().collect();
        for id in uni_ids {
            if let Some(pending) = self.pending_uni_open.get(&id) {
                if let Some(result) = pending.poll() {
                    self.pending_uni_open.remove(&id);
                    if let Ok(stream) = result {
                        if let Ok(writer) = stream.get_writer() {
                            self.uni_send_streams.insert(id, UniSendStreamState { writer });
                        }
                    }
                }
            }
        }

        // Poll pending reads on streams
        for (_id, state) in self.bidi_streams.iter_mut() {
            if let Some(ref pending) = state.pending_read {
                if let Some(result) = pending.poll() {
                    state.pending_read = None;
                    if let Ok(Some(data)) = result {
                        state.read_buffer.push_back(data);
                    }
                }
            }
        }

        for (_id, state) in self.uni_recv_streams.iter_mut() {
            if let Some(ref pending) = state.pending_read {
                if let Some(result) = pending.poll() {
                    state.pending_read = None;
                    if let Ok(Some(data)) = result {
                        state.read_buffer.push_back(data);
                    }
                }
            }
        }
    }

    fn allocate_stream_id(&mut self) -> u32 {
        let id = self.next_stream_id;
        self.next_stream_id += 1;
        id
    }
}

/// Connection context for browser WebTransport operations.
pub struct WebTransportWebConnectionContext<'a> {
    state: &'a mut ConnectedState,
}

impl<'a> WebTransportWebConnectionContext<'a> {
    pub fn new(state: &'a mut ConnectedState) -> Self {
        Self { state }
    }
}

impl<'a> ConnectionContext for WebTransportWebConnectionContext<'a> {
    fn reborrow<'b>(&'b mut self) -> Connection<'b> {
        Connection(Box::new(WebTransportWebConnectionContext {
            state: self.state,
        }))
    }

    fn new_stream(&mut self, requirements: StreamRequirements) -> Result<Stream> {
        let id = self.state.allocate_stream_id();

        match requirements {
            StreamRequirements {
                reliable: false,
                ordered: false,
                ..
            } => {
                // Datagrams
                Ok(Stream::new(WebTransportWebStreamId::Datagrams))
            }
            StreamRequirements {
                bidirectional: true,
                ..
            } => {
                // Open bidirectional stream
                let promise = self.state.transport.create_bidirectional_stream();
                let pending = PendingPromise::new(promise, |v| {
                    v.dyn_into::<WebTransportBidirectionalStream>().unwrap()
                });
                self.state.pending_bidi_open.insert(id, pending);

                Ok(Stream::new(WebTransportWebStreamId::Bidirectional(id)))
            }
            StreamRequirements {
                bidirectional: false,
                ..
            } => {
                // Open unidirectional send stream
                let promise = self.state.transport.create_unidirectional_stream();
                let pending = PendingPromise::new(promise, |v| {
                    v.dyn_into::<WebTransportSendStream>().unwrap()
                });
                self.state.pending_uni_open.insert(id, pending);

                Ok(Stream::new(WebTransportWebStreamId::UnidirectionalSend(id)))
            }
        }
    }

    fn write(&mut self, stream: &Stream, data: Bytes, _block: bool) -> Result<usize> {
        let stream_id = stream.as_stream::<WebTransportWebStreamId>()?;

        match stream_id {
            WebTransportWebStreamId::Datagrams => {
                if let Some(ref writer) = self.state.datagrams_writer {
                    let array = Uint8Array::from(data.as_ref());
                    // Fire and forget write
                    let _ = writer.write_with_chunk(&array);
                    Ok(data.len())
                } else {
                    Ok(0)
                }
            }
            WebTransportWebStreamId::Bidirectional(id) => {
                if let Some(state) = self.state.bidi_streams.get(id) {
                    let array = Uint8Array::from(data.as_ref());
                    let _ = state.send_writer.write_with_chunk(&array);
                    Ok(data.len())
                } else if self.state.pending_bidi_open.contains_key(id) {
                    // Stream is still opening, buffer or return 0
                    Ok(0)
                } else {
                    Ok(0)
                }
            }
            WebTransportWebStreamId::UnidirectionalSend(id) => {
                if let Some(state) = self.state.uni_send_streams.get(id) {
                    let array = Uint8Array::from(data.as_ref());
                    let _ = state.writer.write_with_chunk(&array);
                    Ok(data.len())
                } else if self.state.pending_uni_open.contains_key(id) {
                    Ok(0)
                } else {
                    Ok(0)
                }
            }
            WebTransportWebStreamId::UnidirectionalRecv(_) => {
                // Can't write to receive-only stream
                Ok(0)
            }
            WebTransportWebStreamId::ReceivedDatagram(_) => {
                // Can't write to received datagram
                Ok(0)
            }
        }
    }

    fn read(&mut self, stream: &Stream) -> Result<Result<Bytes, StreamReadError>> {
        let stream_id = stream.as_stream::<WebTransportWebStreamId>()?;

        match stream_id {
            WebTransportWebStreamId::Datagrams => {
                // Can't read from general datagram "stream"
                Err("Use accept_stream to receive datagrams".into())
            }
            WebTransportWebStreamId::Bidirectional(id) => {
                if let Some(state) = self.state.bidi_streams.get_mut(id) {
                    // Check buffer first
                    if let Some(data) = state.read_buffer.pop_front() {
                        return Ok(Ok(Bytes::from(data)));
                    }

                    // Check pending read
                    if let Some(ref pending) = state.pending_read {
                        if let Some(result) = pending.poll() {
                            state.pending_read = None;
                            match result {
                                Ok(Some(data)) => return Ok(Ok(Bytes::from(data))),
                                Ok(None) => return Ok(Err(StreamReadError::Closed)),
                                Err(_) => return Ok(Err(StreamReadError::Closed)),
                            }
                        }
                    }

                    // Start new read if none pending
                    if state.pending_read.is_none() {
                        state.pending_read = Some(PendingRead::new(state.recv_reader.clone()));
                    }

                    Ok(Err(StreamReadError::Blocked))
                } else if self.state.pending_bidi_open.contains_key(id) {
                    Ok(Err(StreamReadError::Blocked))
                } else {
                    Ok(Err(StreamReadError::Closed))
                }
            }
            WebTransportWebStreamId::UnidirectionalSend(_) => {
                // Can't read from send-only stream
                Ok(Err(StreamReadError::Closed))
            }
            WebTransportWebStreamId::UnidirectionalRecv(id) => {
                if let Some(state) = self.state.uni_recv_streams.get_mut(id) {
                    if let Some(data) = state.read_buffer.pop_front() {
                        return Ok(Ok(Bytes::from(data)));
                    }

                    if let Some(ref pending) = state.pending_read {
                        if let Some(result) = pending.poll() {
                            state.pending_read = None;
                            match result {
                                Ok(Some(data)) => return Ok(Ok(Bytes::from(data))),
                                Ok(None) => return Ok(Err(StreamReadError::Closed)),
                                Err(_) => return Ok(Err(StreamReadError::Closed)),
                            }
                        }
                    }

                    if state.pending_read.is_none() {
                        state.pending_read = Some(PendingRead::new(state.reader.clone()));
                    }

                    Ok(Err(StreamReadError::Blocked))
                } else {
                    Ok(Err(StreamReadError::Closed))
                }
            }
            WebTransportWebStreamId::ReceivedDatagram(index) => {
                let idx = index.checked_sub(self.state.datagram_queue_offset);
                if let Some(idx) = idx {
                    if let Some(data) = self.state.received_datagrams.get(idx).cloned() {
                        // Mark as consumed
                        if let Some(slot) = self.state.received_datagrams.get_mut(idx) {
                            *slot = Vec::new();
                        }

                        // Clean up consumed datagrams from front
                        while self.state.received_datagrams.front().map(|v| v.is_empty()).unwrap_or(false) {
                            self.state.received_datagrams.pop_front();
                            self.state.datagram_queue_offset += 1;
                        }

                        if data.is_empty() {
                            Ok(Err(StreamReadError::Closed))
                        } else {
                            Ok(Ok(Bytes::from(data)))
                        }
                    } else {
                        Ok(Err(StreamReadError::Closed))
                    }
                } else {
                    Ok(Err(StreamReadError::Closed))
                }
            }
        }
    }

    fn close_send_stream(&mut self, stream: &Stream, _graceful: bool) -> Result {
        let stream_id = stream.as_stream::<WebTransportWebStreamId>()?;

        match stream_id {
            WebTransportWebStreamId::Bidirectional(id) => {
                if let Some(state) = self.state.bidi_streams.get(id) {
                    let _ = state.send_writer.close();
                }
            }
            WebTransportWebStreamId::UnidirectionalSend(id) => {
                if let Some(state) = self.state.uni_send_streams.get(id) {
                    let _ = state.writer.close();
                }
            }
            _ => {}
        }

        Ok(())
    }

    fn close_recv_stream(&mut self, stream: &Stream) -> Result {
        let stream_id = stream.as_stream::<WebTransportWebStreamId>()?;

        match stream_id {
            WebTransportWebStreamId::Bidirectional(id) => {
                if let Some(state) = self.state.bidi_streams.get(id) {
                    let _ = state.recv_reader.cancel();
                }
            }
            WebTransportWebStreamId::UnidirectionalRecv(id) => {
                if let Some(state) = self.state.uni_recv_streams.get(id) {
                    let _ = state.reader.cancel();
                }
            }
            _ => {}
        }

        Ok(())
    }

    fn accept_stream(&mut self) -> Option<(Stream, StreamRequirements)> {
        // TODO: Poll incoming_bidi_reader and incoming_uni_reader for new streams
        // This requires async handling which we'll need to integrate with the polling pattern

        // Check for received datagrams
        if let Some(ref reader) = self.state.datagrams_reader {
            // We'd need to poll the reader here
            // For now, return None - this needs the promise polling pattern
        }

        None
    }

    fn close(&mut self) {
        self.state.transport.close();
    }

    fn all_data_sent(&mut self) -> bool {
        // Check if all streams have flushed
        true
    }
}
