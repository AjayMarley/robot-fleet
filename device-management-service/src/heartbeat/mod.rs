use std::sync::Arc;

use futures::StreamExt;
use tonic::Status;

use robot_fleet_proto::fleet::v1::{HeartbeatRequest, HeartbeatResponse};

use crate::store::DeviceStore;

pub struct HeartbeatHandler<S: DeviceStore> {
    store: Arc<S>,
}

impl<S: DeviceStore> HeartbeatHandler<S> {
    pub fn new(store: Arc<S>) -> Self {
        Self { store }
    }

    /// Process a heartbeat stream. Extracts `device_id` from the first message,
    /// then processes the remainder of the stream.
    pub async fn handle<St>(
        &self,
        mut stream: St,
        tx: tokio::sync::mpsc::Sender<Result<HeartbeatResponse, Status>>,
    ) where
        St: futures::Stream<Item = Result<HeartbeatRequest, Status>> + Unpin + Send,
    {
        // Extract device_id from first message
        let first = match stream.next().await {
            None => return,
            Some(Err(e)) => {
                let _ = tx.send(Err(Status::internal(e.to_string()))).await;
                return;
            }
            Some(Ok(req)) => req,
        };

        let device_id = first.device_id.clone();
        tracing::info!(device_id, port = 8443, "[Phase 3] heartbeat stream connected — robot is live");
        let combined = futures::stream::once(async move { Ok(first) }).chain(stream);
        futures::pin_mut!(combined);

        while let Some(msg) = combined.next().await {
            match msg {
                Err(e) => {
                    tracing::warn!(device_id, "heartbeat stream error: {e}");
                    break;
                }
                Ok(req) => {
                    tracing::debug!(
                        device_id,
                        cpu = req.cpu_percent,
                        mem = req.memory_percent,
                        bat = req.battery_percent,
                        "heartbeat"
                    );

                    match self.store.touch_last_seen(&device_id).await {
                        Err(crate::error::Error::NotFound(_)) => {
                            let _ = tx
                                .send(Err(Status::not_found(format!(
                                    "device {device_id} not found"
                                ))))
                                .await;
                            return;
                        }
                        Err(e) => tracing::error!(device_id, "touch_last_seen failed: {e}"),
                        Ok(()) => {}
                    }

                    if tx.send(Ok(HeartbeatResponse { throttle: false, directive: String::new() }))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }

        tracing::info!(device_id, "[Phase 3] heartbeat stream disconnected");
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MockDeviceStore;

    fn make_request(cpu: f64) -> HeartbeatRequest {
        HeartbeatRequest {
            device_id: "dev-1".into(),
            cpu_percent: cpu,
            memory_percent: 40.0,
            battery_percent: 80.0,
            recorded_at: None,
            operational_status: "ok".into(),
            extra: Default::default(),
        }
    }

    #[tokio::test]
    async fn heartbeat_touch_last_seen_called_per_message() {
        let mut mock = MockDeviceStore::new();
        mock.expect_touch_last_seen().times(3).returning(|_| Ok(()));

        let handler = HeartbeatHandler::new(Arc::new(mock));
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);

        let stream = futures::stream::iter(vec![
            Ok(make_request(10.0)),
            Ok(make_request(20.0)),
            Ok(make_request(30.0)),
        ]);

        handler.handle(stream, tx).await;

        let responses: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(responses.len(), 3);
        assert!(responses.iter().all(|r| r.is_ok()));
    }

    #[tokio::test]
    async fn heartbeat_not_found_closes_stream_with_error() {
        let mut mock = MockDeviceStore::new();
        mock.expect_touch_last_seen()
            .returning(|id| Err(crate::error::Error::NotFound(id.to_string())));

        let handler = HeartbeatHandler::new(Arc::new(mock));
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);

        let stream = futures::stream::iter(vec![Ok(make_request(50.0))]);
        handler.handle(stream, tx).await;

        let resp = rx.try_recv().unwrap();
        assert!(resp.is_err());
        assert_eq!(resp.unwrap_err().code(), tonic::Code::NotFound);
    }
}
