use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt as _;
use tonic::{Request, Response, Status, Streaming};
use tracing::info;

use robot_fleet_proto::fleet::v1::{
    telemetry_service_server::TelemetryService, TelemetryAck, TelemetryFrame,
};

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
            while let Some(result) = stream.next().await {
                match result {
                    Ok(frame) => {
                        frames_received += 1;
                        info!(
                            device_id = %frame.device_id,
                            joints = frame.joints.len(),
                            frames_received,
                            "telemetry frame received"
                        );
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
        });

        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }
}
