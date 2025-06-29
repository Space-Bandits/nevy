use std::collections::{HashSet, VecDeque};

use quinn_proto::ConnectionStats;
use transport_interface::*;

use crate::{endpoint::QuinnEndpoint, quinn_stream::QuinnStreamId};

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct QuinnConnectionId(pub(crate) quinn_proto::ConnectionHandle);

pub struct QuinnConnection {
    pub(crate) connection: quinn_proto::Connection,
    pub(crate) connection_id: QuinnConnectionId,
    pub(crate) stream_events: VecDeque<StreamEvent<QuinnStreamId>>,
    pub(crate) open_send_streams: HashSet<QuinnStreamId>,
    pub(crate) open_recv_streams: HashSet<QuinnStreamId>,
}

impl QuinnConnection {
    pub(crate) fn new(
        connection: quinn_proto::Connection,
        connection_id: QuinnConnectionId,
    ) -> Self {
        QuinnConnection {
            connection,
            connection_id,
            stream_events: VecDeque::new(),
            open_send_streams: HashSet::new(),
            open_recv_streams: HashSet::new(),
        }
    }

    pub(crate) fn process_event(&mut self, event: quinn_proto::ConnectionEvent) {
        self.connection.handle_event(event);
    }

    pub(crate) fn poll_timeouts(&mut self) {
        let now = std::time::Instant::now();
        while let Some(deadline) = self.connection.poll_timeout() {
            if deadline <= now {
                self.connection.handle_timeout(now);
            } else {
                break;
            }
        }
    }

    pub(crate) fn poll_events(&mut self, handler: &mut impl EndpointEventHandler<QuinnEndpoint>) {
        while let Some(app_event) = self.connection.poll() {
            match app_event {
                quinn_proto::Event::HandshakeDataReady => (),
                quinn_proto::Event::Connected => handler.connected(self.connection_id),
                quinn_proto::Event::ConnectionLost { reason: _ } => {
                    handler.disconnected(self.connection_id);
                }
                quinn_proto::Event::Stream(_s) => {}
                quinn_proto::Event::DatagramReceived => {}
                quinn_proto::Event::DatagramsUnblocked => {}
            }
        }
    }

    pub(crate) fn accept_streams(&mut self) {
        while let Some(stream_id) = self.connection.streams().accept(quinn_proto::Dir::Uni) {
            let stream_id = QuinnStreamId(stream_id);

            self.open_recv_streams.insert(stream_id);

            self.stream_events.push_back(StreamEvent {
                stream_id,
                peer_generated: true,
                event_type: StreamEventType::NewRecvStream,
            });
        }

        while let Some(stream_id) = self.connection.streams().accept(quinn_proto::Dir::Bi) {
            let stream_id = QuinnStreamId(stream_id);

            self.open_recv_streams.insert(stream_id);
            self.open_send_streams.insert(stream_id);

            self.stream_events.push_back(StreamEvent {
                stream_id,
                peer_generated: true,
                event_type: StreamEventType::NewRecvStream,
            });
            self.stream_events.push_back(StreamEvent {
                stream_id,
                peer_generated: true,
                event_type: StreamEventType::NewSendStream,
            });
        }
    }

    pub fn side(&self) -> quinn_proto::Side {
        self.connection.side()
    }
}

impl<'c> ConnectionMut<'c> for &'c mut QuinnConnection {
    type NonMut<'b>
        = &'b QuinnConnection
    where
        Self: 'b;

    type StreamType = QuinnStreamId;

    fn as_ref<'b>(&'b self) -> Self::NonMut<'b> {
        self
    }

    fn disconnect(&mut self) {
        self.connection.close(
            std::time::Instant::now(),
            Default::default(),
            Default::default(),
        );
    }
}

impl<'c> ConnectionRef<'c> for &'c QuinnConnection {
    type ConnectionStats = (std::net::SocketAddr, ConnectionStats);

    fn get_stats(&self) -> (std::net::SocketAddr, ConnectionStats) {
        (self.connection.remote_address(), self.connection.stats())
    }
}
