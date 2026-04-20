use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{broadcast, RwLock};

use robot_fleet_proto::fleet::v1::UpdateCommand;

use crate::error::{Error, Result};

const CHANNEL_CAPACITY: usize = 8;

/// Broadcasts OTA update commands to connected robot-agent subscribers.
/// Each device gets its own broadcast channel; dropped receivers are cleaned up automatically.
#[derive(Clone, Default)]
pub struct Dispatcher {
    senders: Arc<RwLock<HashMap<String, broadcast::Sender<UpdateCommand>>>>,
}

impl Dispatcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a receiver for the device's broadcast channel, creating it if absent.
    pub async fn subscribe(&self, device_id: &str) -> broadcast::Receiver<UpdateCommand> {
        let mut map = self.senders.write().await;
        let sender = map
            .entry(device_id.to_string())
            .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0);
        sender.subscribe()
    }

    /// Sends an UpdateCommand to all active subscribers for the device.
    /// Returns the number of receivers that got the message.
    /// Returns `Error::NoActiveSubscribers` if no robot is connected.
    pub async fn dispatch(&self, device_id: &str, cmd: UpdateCommand) -> Result<usize> {
        let map = self.senders.read().await;
        match map.get(device_id) {
            None => Err(Error::NoActiveSubscribers(device_id.to_string())),
            Some(sender) => {
                let count = sender.receiver_count();
                if count == 0 {
                    return Err(Error::NoActiveSubscribers(device_id.to_string()));
                }
                sender.send(cmd).ok();
                Ok(count)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(version: &str) -> UpdateCommand {
        UpdateCommand {
            update_job_id: "job-1".to_string(),
            target_version: version.to_string(),
            artifact_url: "http://example.com/v.tar.gz".to_string(),
            sha256: "abc".to_string(),
            size_bytes: 0,
            apply_by: None,
        }
    }

    #[tokio::test]
    async fn subscribe_and_dispatch_delivers_command() {
        let d = Dispatcher::new();
        let mut rx = d.subscribe("dev-1").await;
        let count = d.dispatch("dev-1", cmd("v2.0.0")).await.unwrap();
        assert_eq!(count, 1);
        let received = rx.recv().await.unwrap();
        assert_eq!(received.target_version, "v2.0.0");
    }

    #[tokio::test]
    async fn dispatch_with_no_subscribers_returns_error() {
        let d = Dispatcher::new();
        let err = d.dispatch("dev-ghost", cmd("v2.0.0")).await.unwrap_err();
        assert!(matches!(err, Error::NoActiveSubscribers(_)));
    }

    #[tokio::test]
    async fn dispatch_after_receiver_dropped_returns_error() {
        let d = Dispatcher::new();
        let rx = d.subscribe("dev-1").await;
        drop(rx);
        let err = d.dispatch("dev-1", cmd("v2.0.0")).await.unwrap_err();
        assert!(matches!(err, Error::NoActiveSubscribers(_)));
    }

    #[tokio::test]
    async fn multiple_subscribers_all_receive() {
        let d = Dispatcher::new();
        let mut rx1 = d.subscribe("dev-1").await;
        let mut rx2 = d.subscribe("dev-1").await;
        let count = d.dispatch("dev-1", cmd("v2.0.0")).await.unwrap();
        assert_eq!(count, 2);
        assert_eq!(rx1.recv().await.unwrap().target_version, "v2.0.0");
        assert_eq!(rx2.recv().await.unwrap().target_version, "v2.0.0");
    }

    #[tokio::test]
    async fn dispatch_is_independent_per_device() {
        let d = Dispatcher::new();
        let mut rx1 = d.subscribe("dev-1").await;
        let _rx2 = d.subscribe("dev-2").await;

        d.dispatch("dev-1", cmd("v2.0.0")).await.unwrap();

        // dev-2 gets nothing
        assert!(rx1.recv().await.is_ok());
        let err = d.dispatch("dev-99", cmd("v2.0.0")).await.unwrap_err();
        assert!(matches!(err, Error::NoActiveSubscribers(_)));
    }

    #[tokio::test]
    async fn subscribe_creates_channel_on_first_call() {
        let d = Dispatcher::new();
        let _rx = d.subscribe("new-device").await;
        // Should now have a channel — dispatch should not return NoActiveSubscribers
        let result = d.dispatch("new-device", cmd("v1.0.0")).await;
        assert!(result.is_ok());
    }
}
