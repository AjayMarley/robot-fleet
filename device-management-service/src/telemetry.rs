use std::time::{Duration, Instant};

use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt as _;
use tonic::{Request, Response, Status, Streaming};
use tracing::info;

use robot_fleet_proto::fleet::v1::{
    telemetry_service_server::TelemetryService, TelemetryAck, TelemetryFrame,
};

const LOG_INTERVAL: Duration = Duration::from_secs(5);

pub struct TelemetryServer;

type BoxStream<T> = std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Result<T, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl TelemetryService for TelemetryServer {
    type StreamTelemetryStream = BoxStream<TelemetryAck>;

    async fn stream_telemetry(
        &self,
        request: Request<Streaming<TelemetryFrame>>,
    ) -> Result<Response<Self::StreamTelemetryStream>, Status> {
        let mut stream = request.into_inner();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<TelemetryAck, Status>>(32);

        tokio::spawn(async move {
            let mut frames_received: u64 = 0;
            let mut frames_in_window: u64 = 0;
            let mut last_log = Instant::now();
            let mut device_id = String::new();

            while let Some(result) = stream.next().await {
                match result {
                    Ok(frame) => {
                        frames_received += 1;
                        frames_in_window += 1;
                        device_id = frame.device_id.clone();

                        if last_log.elapsed() >= LOG_INTERVAL {
                            info!(
                                device_id        = %device_id,
                                frames_in_window,
                                frames_total     = frames_received,
                                fps              = frames_in_window / LOG_INTERVAL.as_secs(),
                                port             = 8443,
                                "[Phase 3] telemetry stream active"
                            );
                            frames_in_window = 0;
                            last_log = Instant::now();
                        }

                        if tx.send(Ok(TelemetryAck { frames_received, throttle: false })).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(Status::internal(e.to_string()))).await;
                        break;
                    }
                }
            }

            if frames_in_window > 0 {
                info!(
                    device_id    = %device_id,
                    frames_in_window,
                    frames_total = frames_received,
                    "[Phase 3] telemetry stream ended"
                );
            }
        });

        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }
}
