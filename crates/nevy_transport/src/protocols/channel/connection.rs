//! Channel connection context implementation.

use std::{any::Any, collections::VecDeque, time::{Duration, Instant}};

use bytes::Bytes;
use crossbeam_channel::{Receiver, Sender, TrySendError};

use crate::{
    Connection, ConnectionContext, Stream, StreamId, StreamReadError, StreamRequirements,
};

use super::registry::ChannelMessage;

/// Configuration for simulating adverse network conditions on a channel connection.
///
/// When set on a [`ChannelConnectionConfig`](super::endpoint::ChannelConnectionConfig),
/// received messages will be delayed, dropped, or duplicated according to these parameters.
/// When `None`, messages pass through with zero overhead.
///
/// The conditioner affects the **receive side** of this connection — it simulates what
/// would happen to data traveling over an imperfect network before it reaches you.
#[derive(Clone, Debug)]
pub struct LinkConditionerConfig {
    /// One-way latency in milliseconds added to every message.
    pub latency_ms: u32,
    /// Random jitter applied uniformly in the range `[-jitter_ms, +jitter_ms]`,
    /// added on top of `latency_ms`. The effective delay is clamped to a minimum of 0.
    pub jitter_ms: u32,
    /// Probability of dropping a message outright. `0.0` = no loss, `1.0` = drop everything.
    pub packet_loss: f64,
    /// Probability of duplicating a message. `0.0` = no duplicates, `1.0` = duplicate everything.
    /// Duplicates receive their own independent delay.
    pub duplicate: f64,
    /// Optional seed for deterministic behavior. If `None`, uses a time-based seed.
    pub seed: Option<u64>,
}

impl Default for LinkConditionerConfig {
    fn default() -> Self {
        Self {
            latency_ms: 0,
            jitter_ms: 0,
            packet_loss: 0.0,
            duplicate: 0.0,
            seed: None,
        }
    }
}

/// Simple xorshift64 RNG — no external dependency needed.
struct SimpleRng {
    state: u64,
}

impl SimpleRng {
    fn new(seed: u64) -> Self {
        // Ensure non-zero state
        Self { state: if seed == 0 { 0x12345678_9abcdef0 } else { seed } }
    }

    fn next_u64(&mut self) -> u64 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        self.state
    }

    /// Returns a float in `[0.0, 1.0)`.
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Returns a value in `[-range, +range]` (inclusive endpoints are approximate).
    fn next_jitter(&mut self, range: u32) -> i64 {
        if range == 0 {
            return 0;
        }
        let span = range as u64 * 2 + 1;
        let val = self.next_u64() % span;
        val as i64 - range as i64
    }
}

/// Internal state for the link conditioner.
pub(crate) struct LinkConditionerState {
    config: LinkConditionerConfig,
    delayed: VecDeque<(Instant, ChannelMessage)>,
    rng: SimpleRng,
}

impl LinkConditionerState {
    fn new(config: LinkConditionerConfig) -> Self {
        let seed = config.seed.unwrap_or_else(|| {
            // Time-based seed
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(42)
        });
        Self {
            config,
            delayed: VecDeque::new(),
            rng: SimpleRng::new(seed),
        }
    }

    /// Decide whether to enqueue a message (applying loss/duplication/delay).
    fn enqueue(&mut self, msg: ChannelMessage, now: Instant) {
        // Packet loss check
        if self.rng.next_f64() < self.config.packet_loss {
            return; // Dropped
        }

        // Compute delivery time
        let base_delay = self.config.latency_ms as i64;
        let jitter = self.rng.next_jitter(self.config.jitter_ms);
        let delay_ms = (base_delay + jitter).max(0) as u64;
        let deliver_at = now + Duration::from_millis(delay_ms);

        // Check for duplication *before* consuming the message
        let should_duplicate = self.rng.next_f64() < self.config.duplicate;

        if should_duplicate {
            // Duplicate gets its own independent jitter
            let dup_jitter = self.rng.next_jitter(self.config.jitter_ms);
            let dup_delay_ms = (base_delay + dup_jitter).max(0) as u64;
            let dup_deliver_at = now + Duration::from_millis(dup_delay_ms);
            self.insert_sorted(dup_deliver_at, msg.clone());
        }

        self.insert_sorted(deliver_at, msg);
    }

    /// Insert a message into the delayed queue in sorted order (earliest first).
    fn insert_sorted(&mut self, deliver_at: Instant, msg: ChannelMessage) {
        let pos = self.delayed.iter().position(|(t, _)| *t > deliver_at)
            .unwrap_or(self.delayed.len());
        self.delayed.insert(pos, (deliver_at, msg));
    }

    /// Drain messages that are ready for delivery.
    fn drain_ready(&mut self, now: Instant) -> Vec<ChannelMessage> {
        let mut ready = Vec::new();
        while let Some((deliver_at, _)) = self.delayed.front() {
            if *deliver_at <= now {
                ready.push(self.delayed.pop_front().unwrap().1);
            } else {
                break;
            }
        }
        ready
    }
}

/// Internal state for a channel connection.
pub(crate) struct ChannelConnectionState {
    /// Channel to send messages to the remote.
    pub tx: Sender<ChannelMessage>,
    /// Channel to receive messages from the remote.
    pub rx: Receiver<ChannelMessage>,
    /// State of streams on this connection.
    pub streams: bevy::platform::collections::HashMap<u32, StreamState>,
    /// Next stream ID to allocate (locally initiated).
    pub next_stream_id: u32,
    /// Queue of incoming streams waiting to be accepted.
    pub incoming_streams: VecDeque<(u32, StreamRequirements)>,
    /// Queue of received datagrams.
    pub datagrams: VecDeque<Bytes>,
    /// Index for tracking datagram streams.
    pub datagram_index: usize,
    /// Whether close has been requested.
    pub close_requested: bool,
    /// Optional link conditioner for simulating network conditions.
    conditioner: Option<LinkConditionerState>,
}

impl ChannelConnectionState {
    pub fn new(
        tx: Sender<ChannelMessage>,
        rx: Receiver<ChannelMessage>,
        conditioner_config: Option<LinkConditionerConfig>,
    ) -> Self {
        Self {
            tx,
            rx,
            streams: bevy::platform::collections::HashMap::new(),
            next_stream_id: 0,
            incoming_streams: VecDeque::new(),
            datagrams: VecDeque::new(),
            datagram_index: 0,
            close_requested: false,
            conditioner: conditioner_config.map(LinkConditionerState::new),
        }
    }

    /// Process all pending messages from the remote.
    pub fn poll_messages(&mut self) {
        let now = Instant::now();

        // Receive from channel — either buffer through conditioner or process directly
        while let Ok(msg) = self.rx.try_recv() {
            if let Some(ref mut conditioner) = self.conditioner {
                conditioner.enqueue(msg, now);
            } else {
                self.process_message(msg);
            }
        }

        // Release any delayed messages that are ready
        if let Some(ref mut conditioner) = self.conditioner {
            for msg in conditioner.drain_ready(now) {
                self.process_message(msg);
            }
        }
    }

    /// Process a single message into the connection state.
    fn process_message(&mut self, msg: ChannelMessage) {
        match msg {
            ChannelMessage::StreamOpen {
                stream_id,
                requirements,
            } => {
                self.streams.insert(
                    stream_id,
                    StreamState {
                        recv_buffer: VecDeque::new(),
                        send_closed: false,
                        recv_closed: false,
                    },
                );
                self.incoming_streams.push_back((stream_id, requirements));
            }
            ChannelMessage::StreamData { stream_id, data } => {
                if let Some(stream) = self.streams.get_mut(&stream_id) {
                    stream.recv_buffer.push_back(data);
                }
            }
            ChannelMessage::StreamClose { stream_id } => {
                if let Some(stream) = self.streams.get_mut(&stream_id) {
                    stream.recv_closed = true;
                }
            }
            ChannelMessage::Datagram(data) => {
                self.datagrams.push_back(data);
            }
        }
    }
}

/// State for a single stream.
pub(crate) struct StreamState {
    /// Buffer of received data chunks.
    pub recv_buffer: VecDeque<Bytes>,
    /// Whether the send side is closed.
    pub send_closed: bool,
    /// Whether the receive side is closed.
    pub recv_closed: bool,
}

/// Stream ID for channel transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChannelStreamId {
    /// A regular stream with an ID.
    Stream(u32),
    /// Stream for sending datagrams.
    Datagram,
    /// A received datagram (each datagram is treated as its own "stream").
    ReceivedDatagram(usize),
}

impl StreamId for ChannelStreamId {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn clone(&self) -> Stream {
        Stream::new(<Self as Clone>::clone(self))
    }

    fn eq(&self, rhs: &Stream) -> bool {
        match (self, rhs.as_stream::<Self>()) {
            (_, Err(_)) => false,
            (Self::Datagram, Ok(Self::Datagram)) => true,
            (Self::Stream(id1), Ok(Self::Stream(id2))) => id1 == id2,
            (Self::ReceivedDatagram(i1), Ok(Self::ReceivedDatagram(i2))) => i1 == i2,
            _ => false,
        }
    }
}

/// Connection context for channel transport.
pub struct ChannelConnectionContext<'a> {
    pub(crate) state: &'a mut ChannelConnectionState,
}

impl<'a> ConnectionContext for ChannelConnectionContext<'a> {
    fn reborrow<'b>(&'b mut self) -> Connection<'b> {
        Connection(Box::new(ChannelConnectionContext { state: self.state }))
    }

    fn new_stream(&mut self, requirements: StreamRequirements) -> crate::Result<Stream> {
        // For unreliable unordered, use datagram
        if !requirements.reliable && !requirements.ordered {
            return Ok(Stream::new(ChannelStreamId::Datagram));
        }

        let stream_id = self.state.next_stream_id;
        self.state.next_stream_id += 1;

        // Send stream open message to remote
        let _ = self.state.tx.try_send(ChannelMessage::StreamOpen {
            stream_id,
            requirements,
        });

        self.state.streams.insert(
            stream_id,
            StreamState {
                recv_buffer: VecDeque::new(),
                send_closed: false,
                recv_closed: false,
            },
        );

        Ok(Stream::new(ChannelStreamId::Stream(stream_id)))
    }

    fn write(&mut self, stream: &Stream, data: Bytes, _block: bool) -> crate::Result<usize> {
        let stream_id = stream.as_stream::<ChannelStreamId>()?;

        match stream_id {
            ChannelStreamId::Datagram => {
                let len = data.len();
                match self.state.tx.try_send(ChannelMessage::Datagram(data)) {
                    Ok(()) => Ok(len),
                    Err(TrySendError::Full(_)) => Ok(0),
                    Err(TrySendError::Disconnected(_)) => Ok(0),
                }
            }
            &ChannelStreamId::Stream(id) => {
                let len = data.len();
                match self.state.tx.try_send(ChannelMessage::StreamData {
                    stream_id: id,
                    data,
                }) {
                    Ok(()) => Ok(len),
                    Err(TrySendError::Full(_)) => Ok(0),
                    Err(TrySendError::Disconnected(_)) => Ok(0),
                }
            }
            ChannelStreamId::ReceivedDatagram(_) => {
                // Can't write to received datagram streams
                Ok(0)
            }
        }
    }

    fn read(&mut self, stream: &Stream) -> crate::Result<Result<Bytes, StreamReadError>> {
        let stream_id = stream.as_stream::<ChannelStreamId>()?;

        match stream_id {
            ChannelStreamId::Datagram => {
                // Datagram send stream can't be read
                Ok(Err(StreamReadError::Blocked))
            }
            &ChannelStreamId::Stream(id) => {
                let Some(stream_state) = self.state.streams.get_mut(&id) else {
                    return Ok(Err(StreamReadError::Closed));
                };

                if let Some(data) = stream_state.recv_buffer.pop_front() {
                    return Ok(Ok(data));
                }

                if stream_state.recv_closed {
                    Ok(Err(StreamReadError::Closed))
                } else {
                    Ok(Err(StreamReadError::Blocked))
                }
            }
            &ChannelStreamId::ReceivedDatagram(index) => {
                // Check if this datagram has already been read
                if index < self.state.datagram_index {
                    return Ok(Err(StreamReadError::Closed));
                }

                let relative_index = index - self.state.datagram_index;
                if relative_index < self.state.datagrams.len() {
                    // Remove all datagrams up to and including this one
                    for _ in 0..=relative_index {
                        self.state.datagrams.pop_front();
                    }
                    self.state.datagram_index = index + 1;
                    // Note: The actual data was consumed, return closed since it's a one-shot read
                    Ok(Err(StreamReadError::Closed))
                } else {
                    Ok(Err(StreamReadError::Blocked))
                }
            }
        }
    }

    fn close_send_stream(&mut self, stream: &Stream, _graceful: bool) -> crate::Result {
        let stream_id = stream.as_stream::<ChannelStreamId>()?;

        if let &ChannelStreamId::Stream(id) = stream_id {
            let _ = self
                .state
                .tx
                .try_send(ChannelMessage::StreamClose { stream_id: id });

            if let Some(stream_state) = self.state.streams.get_mut(&id) {
                stream_state.send_closed = true;
            }
        }

        Ok(())
    }

    fn close_recv_stream(&mut self, stream: &Stream) -> crate::Result {
        let stream_id = stream.as_stream::<ChannelStreamId>()?;

        if let &ChannelStreamId::Stream(id) = stream_id {
            if let Some(stream_state) = self.state.streams.get_mut(&id) {
                stream_state.recv_closed = true;
            }
        }

        Ok(())
    }

    fn accept_stream(&mut self) -> Option<(Stream, StreamRequirements)> {
        // First check for incoming streams
        if let Some((stream_id, requirements)) = self.state.incoming_streams.pop_front() {
            return Some((Stream::new(ChannelStreamId::Stream(stream_id)), requirements));
        }

        // Then check for datagrams
        if let Some(data) = self.state.datagrams.pop_front() {
            let stream_index = self.state.datagram_index;
            self.state.datagram_index += 1;

            // Store the datagram data in a special stream so it can be read
            let special_id = u32::MAX - (stream_index as u32 % u32::MAX);
            self.state.streams.insert(
                special_id,
                StreamState {
                    recv_buffer: {
                        let mut buf = VecDeque::new();
                        buf.push_back(data);
                        buf
                    },
                    send_closed: true,
                    recv_closed: false,
                },
            );
            return Some((
                Stream::new(ChannelStreamId::Stream(special_id)),
                StreamRequirements::UNRELIABLE,
            ));
        }

        None
    }

    fn close(&mut self) {
        self.state.close_requested = true;
    }

    fn all_data_sent(&mut self) -> bool {
        // For channel transport, data is sent immediately
        true
    }
}
