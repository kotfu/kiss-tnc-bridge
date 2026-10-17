// Copyright (C) 2026 Jared Crapo
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! The event hub: bridges and the daemon report what happens, and control
//! connections that subscribed get it.
//!
//! Nothing here slows the data path when nobody is listening: frames are
//! decoded and copied only while someone subscribes to `frames`, activity
//! is counted only while someone subscribes to `activity`, and every send
//! is non-blocking. Activity is coalesced to about 10 events a second per
//! port or session; a TUI flashing a square doesn't need more.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::{broadcast, mpsc};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::protocol::{Dir, Event, Topic, rfc3339};
use crate::bridge::SessionId;

/// Events a slow subscriber can fall behind by before it misses some.
const BACKLOG: usize = 256;
/// At most one activity event per port or session this often.
pub const ACTIVITY_INTERVAL: Duration = Duration::from_millis(100);

pub struct Hub {
    state: broadcast::Sender<Event>,
    activity: broadcast::Sender<Event>,
    frames: broadcast::Sender<Event>,
    raw_activity: mpsc::Sender<(String, Dir, u64)>,
}

impl Hub {
    /// A hub, with its activity coalescer running until `shutdown`.
    pub fn new(shutdown: CancellationToken) -> Arc<Hub> {
        let (raw_activity, raw_rx) = mpsc::channel(1024);
        let hub = Arc::new(Hub {
            state: broadcast::channel(BACKLOG).0,
            activity: broadcast::channel(BACKLOG).0,
            frames: broadcast::channel(BACKLOG).0,
            raw_activity,
        });
        tokio::spawn(coalesce(raw_rx, hub.activity.clone(), shutdown));
        hub
    }

    pub fn subscribe(&self, topic: Topic) -> broadcast::Receiver<Event> {
        match topic {
            Topic::State => self.state.subscribe(),
            Topic::Activity => self.activity.subscribe(),
            Topic::Frames => self.frames.subscribe(),
        }
    }

    /// `what` is `bridge`, `tnc`, `session`, `port` or `config`.
    pub fn state(&self, what: &str, object: impl Into<String>, state: impl Into<String>) {
        if self.state.receiver_count() > 0 {
            let _ = self.state.send(Event::State {
                what: what.into(),
                object: object.into(),
                state: state.into(),
            });
        }
    }

    pub fn wants_activity(&self) -> bool {
        self.activity.receiver_count() > 0
    }

    /// Count bytes on `object`; only call when [`wants_activity`](Self::wants_activity).
    pub fn activity(&self, object: String, dir: Dir, bytes: usize) {
        let _ = self.raw_activity.try_send((object, dir, bytes as u64));
    }

    pub fn wants_frames(&self) -> bool {
        self.frames.receiver_count() > 0
    }

    /// A frame to or from a TNC; only call when [`wants_frames`](Self::wants_frames).
    pub fn frame(&self, object: String, dir: Dir, session: Option<&SessionId>, raw: &[u8]) {
        let _ = self.frames.send(Event::Frame {
            object,
            dir,
            session: session.map(|s| s.to_string()),
            at: rfc3339(SystemTime::now()),
            kiss: raw.iter().map(|b| format!("{b:02x}")).collect(),
            decoded: crate::ax25::decode(raw),
        });
    }
}

/// Sum bytes per object and direction, and emit at most one event per
/// [`ACTIVITY_INTERVAL`] for each.
async fn coalesce(
    mut raw: mpsc::Receiver<(String, Dir, u64)>,
    out: broadcast::Sender<Event>,
    shutdown: CancellationToken,
) {
    struct Slot {
        last: Instant,
        pending: u64,
    }
    let mut slots: HashMap<(String, Dir), Slot> = HashMap::new();
    let mut tick = tokio::time::interval(ACTIVITY_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let emit = |object: &str, dir: Dir, bytes: u64| {
        let _ = out.send(Event::Activity {
            object: object.to_string(),
            dir,
            bytes,
        });
    };
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            got = raw.recv() => {
                let Some((object, dir, bytes)) = got else { return };
                let now = Instant::now();
                let slot = slots.entry((object.clone(), dir)).or_insert(Slot {
                    last: now - ACTIVITY_INTERVAL,
                    pending: 0,
                });
                slot.pending += bytes;
                if now.duration_since(slot.last) >= ACTIVITY_INTERVAL {
                    emit(&object, dir, slot.pending);
                    slot.pending = 0;
                    slot.last = now;
                }
            }
            _ = tick.tick() => {
                let now = Instant::now();
                for ((object, dir), slot) in slots.iter_mut() {
                    if slot.pending > 0 && now.duration_since(slot.last) >= ACTIVITY_INTERVAL {
                        emit(object, *dir, slot.pending);
                        slot.pending = 0;
                        slot.last = now;
                    }
                }
                // Forget objects that have been quiet a while.
                slots.retain(|_, s| s.pending > 0 || now.duration_since(s.last) < Duration::from_secs(60));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn nothing_is_sent_without_subscribers() {
        let hub = Hub::new(CancellationToken::new());
        assert!(!hub.wants_frames());
        assert!(!hub.wants_activity());
        hub.state("bridge", "ktbr0", "started"); // no receivers: dropped
        let mut rx = hub.subscribe(Topic::State);
        hub.state("bridge", "ktbr0", "stopped");
        assert_eq!(
            rx.recv().await.unwrap(),
            Event::State {
                what: "bridge".into(),
                object: "ktbr0".into(),
                state: "stopped".into()
            }
        );
        let _frames = hub.subscribe(Topic::Frames);
        assert!(hub.wants_frames());
    }

    #[tokio::test(start_paused = true)]
    async fn activity_is_coalesced_per_object() {
        let hub = Hub::new(CancellationToken::new());
        let mut rx = hub.subscribe(Topic::Activity);
        // A burst: the first goes out at once, the rest are summed.
        for _ in 0..10 {
            hub.activity("ktbr0/radio".into(), Dir::Rx, 10);
        }
        hub.activity("ktbr0/lan/a".into(), Dir::Tx, 5);
        let mut got = Vec::new();
        for _ in 0..3 {
            got.push(
                tokio::time::timeout(Duration::from_secs(1), rx.recv())
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        let radio: Vec<u64> = got
            .iter()
            .filter_map(|e| match e {
                Event::Activity { object, bytes, .. } if object == "ktbr0/radio" => Some(*bytes),
                _ => None,
            })
            .collect();
        assert_eq!(radio, vec![10, 90]);
        assert!(got.iter().any(|e| matches!(e, Event::Activity { object, bytes: 5, dir: Dir::Tx } if object == "ktbr0/lan/a")));
        // Quiet afterwards.
        assert!(
            tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn frames_carry_the_decoded_header() {
        let hub = Hub::new(CancellationToken::new());
        let mut rx = hub.subscribe(Topic::Frames);
        hub.frame(
            "ktbr0/radio".into(),
            Dir::Tx,
            Some(&SessionId::new("lan", "x:1")),
            &[0x01, 0x32],
        );
        match rx.recv().await.unwrap() {
            Event::Frame {
                object,
                session,
                kiss,
                decoded,
                ..
            } => {
                assert_eq!(object, "ktbr0/radio");
                assert_eq!(session.as_deref(), Some("lan/x:1"));
                assert_eq!(kiss, "0132");
                assert_eq!(decoded.command, 1);
            }
            other => panic!("{other:?}"),
        }
    }
}
