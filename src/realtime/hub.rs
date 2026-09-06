//! Process-local realtime connection registry.

use std::{num::NonZeroUsize, sync::Arc};

use dashmap::DashMap;
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

use crate::domain::{ActorId, ActorRef, OrganizationId};

/// Small wakeup hint; the durable stream owns the actual message payload.
#[derive(Clone, Debug)]
pub struct DeliveryWakeup {
    /// Actor whose stream advanced.
    pub actor_id: ActorId,
    /// Highest position observed by this wakeup.
    pub sequence: i64,
}

type ActorConnections = DashMap<Uuid, mpsc::Sender<Arc<DeliveryWakeup>>>;

/// Fully scoped destination for one represented actor stream.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RealtimeTarget {
    /// Organization owning the stream.
    pub organization_id: OrganizationId,
    /// Typed actor owning the stream.
    pub actor: ActorRef,
}

/// Cloneable registry of active WebSocket connections indexed by actor.
#[derive(Clone, Default)]
pub struct RealtimeHub {
    actors: Arc<DashMap<RealtimeTarget, Arc<ActorConnections>>>,
    authorization: watch::Sender<u64>,
    disconnect: watch::Sender<Option<String>>,
}

/// One connection registration and its bounded outbound queue.
pub struct HubRegistration {
    connection_id: Uuid,
    targets: Vec<RealtimeTarget>,
    receiver: mpsc::Receiver<Arc<DeliveryWakeup>>,
    hub: RealtimeHub,
}

/// Result of a non-blocking local fan-out attempt.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PublishReport {
    /// Connections considered.
    pub attempted: usize,
    /// Frames accepted by outbound queues.
    pub enqueued: usize,
    /// Full queues left for durable replay.
    pub saturated: usize,
    /// Closed queues removed from the registry.
    pub closed: usize,
}

impl RealtimeHub {
    /// Asks every local socket to revalidate its own token with IAM immediately.
    pub fn invalidate_authorization(&self) {
        self.authorization
            .send_modify(|version| *version = version.wrapping_add(1));
    }

    /// Closes all current sockets when a testing environment changes lifecycle.
    pub fn disconnect_all(&self, reason: &str) {
        self.disconnect.send_replace(Some(reason.to_owned()));
    }

    /// Subscribes to authorization revalidation requests for this data plane.
    pub(crate) fn authorization_changes(&self) -> watch::Receiver<u64> {
        self.authorization.subscribe()
    }

    /// Subscribes to forced connection closure for this data plane.
    pub(crate) fn disconnects(&self) -> watch::Receiver<Option<String>> {
        self.disconnect.subscribe()
    }

    /// Registers a bounded connection for one or more IAM-authorized actors.
    ///
    /// Duplicate actor IDs are canonicalized. Callers must keep the returned
    /// registration alive for the lifetime of the socket; dropping it removes
    /// every index entry.
    #[must_use]
    pub fn register<I>(&self, targets: I, capacity: NonZeroUsize) -> HubRegistration
    where
        I: IntoIterator<Item = RealtimeTarget>,
    {
        let mut targets = targets.into_iter().collect::<Vec<_>>();
        targets.sort_by(|left, right| {
            left.organization_id
                .cmp(&right.organization_id)
                .then_with(|| {
                    left.actor
                        .actor_type
                        .as_str()
                        .cmp(right.actor.actor_type.as_str())
                })
                .then_with(|| left.actor.id.cmp(&right.actor.id))
        });
        targets.dedup();
        let connection_id = Uuid::now_v7();
        let (sender, receiver) = mpsc::channel(capacity.get());

        for target in &targets {
            let connections = self
                .actors
                .entry(target.clone())
                .or_insert_with(|| Arc::new(DashMap::new()))
                .clone();
            connections.insert(connection_id, sender.clone());
        }

        HubRegistration {
            connection_id,
            targets,
            receiver,
            hub: self.clone(),
        }
    }

    /// Wakes every local connection without retaining message content.
    ///
    /// Saturation is intentionally non-blocking: the durable actor stream
    /// remains authoritative and the session will replay the frame.
    #[must_use]
    pub fn publish(&self, target: &RealtimeTarget, frame: &DeliveryWakeup) -> PublishReport {
        let mut report = PublishReport::default();
        let Some(connections) = self.actors.get(target).map(|entry| entry.clone()) else {
            return report;
        };
        let mut closed = Vec::new();
        let frame = Arc::new(frame.clone());

        for connection in connections.iter() {
            report.attempted += 1;
            match connection.value().try_send(Arc::clone(&frame)) {
                Ok(()) => report.enqueued += 1,
                Err(mpsc::error::TrySendError::Full(_)) => report.saturated += 1,
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    report.closed += 1;
                    closed.push(*connection.key());
                }
            }
        }
        for connection_id in closed {
            connections.remove(&connection_id);
        }
        if connections.is_empty() {
            self.actors.remove(target);
        }
        report
    }

    /// Number of local sockets currently registered for an actor.
    #[must_use]
    pub fn connection_count(&self, target: &RealtimeTarget) -> usize {
        self.actors
            .get(target)
            .map_or(0, |connections| connections.len())
    }

    /// Returns a point-in-time snapshot of locally connected actor streams.
    #[must_use]
    pub fn connected_targets(&self) -> Vec<RealtimeTarget> {
        self.actors
            .iter()
            .map(|entry| entry.key().clone())
            .collect()
    }

    fn unregister(&self, connection_id: Uuid, targets: &[RealtimeTarget]) {
        for target in targets {
            let remove_actor = self.actors.get(target).is_some_and(|connections| {
                connections.remove(&connection_id);
                connections.is_empty()
            });
            if remove_actor {
                self.actors.remove(target);
            }
        }
    }
}

impl HubRegistration {
    /// Unique connection identifier included in the ready frame.
    #[must_use]
    pub fn connection_id(&self) -> Uuid {
        self.connection_id
    }

    /// Canonically ordered represented actors.
    #[must_use]
    pub fn targets(&self) -> &[RealtimeTarget] {
        &self.targets
    }

    /// Receives the next wakeup accepted by this connection's local queue.
    pub async fn recv(&mut self) -> Option<Arc<DeliveryWakeup>> {
        self.receiver.recv().await
    }
}

impl Drop for HubRegistration {
    fn drop(&mut self) {
        self.hub.unregister(self.connection_id, &self.targets);
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use super::RealtimeHub;
    use crate::{
        domain::{ActorRef, ActorType},
        realtime::DeliveryWakeup,
    };

    fn target() -> Result<super::RealtimeTarget, Box<dyn std::error::Error>> {
        Ok(super::RealtimeTarget {
            organization_id: "org-1".parse()?,
            actor: ActorRef {
                actor_type: ActorType::Carbon,
                id: "carbon-1".parse()?,
            },
        })
    }

    #[tokio::test]
    async fn publish_fans_out_and_drop_unregisters() -> Result<(), Box<dyn std::error::Error>> {
        let hub = RealtimeHub::default();
        let actor = target()?;
        let capacity = NonZeroUsize::new(2).ok_or("test capacity must be non-zero")?;
        let mut first = hub.register([actor.clone()], capacity);
        let mut second = hub.register([actor.clone()], capacity);
        let frame = DeliveryWakeup {
            actor_id: actor.actor.id.clone(),
            sequence: 1,
        };

        let report = hub.publish(&actor, &frame);
        assert_eq!(report.enqueued, 2);
        assert!(first.recv().await.is_some());
        assert!(second.recv().await.is_some());
        drop(first);
        assert_eq!(hub.connection_count(&actor), 1);
        Ok(())
    }

    #[tokio::test]
    async fn saturation_never_blocks_the_publisher() -> Result<(), Box<dyn std::error::Error>> {
        let hub = RealtimeHub::default();
        let mut actor = target()?;
        actor.actor.actor_type = ActorType::Silicon;
        actor.actor.id = "silicon-1".parse()?;
        let capacity = NonZeroUsize::new(1).ok_or("test capacity must be non-zero")?;
        let _registration = hub.register([actor.clone()], capacity);
        let frame = DeliveryWakeup {
            actor_id: actor.actor.id.clone(),
            sequence: 1,
        };

        assert_eq!(hub.publish(&actor, &frame).enqueued, 1);
        assert_eq!(hub.publish(&actor, &frame).saturated, 1);
        Ok(())
    }
}
