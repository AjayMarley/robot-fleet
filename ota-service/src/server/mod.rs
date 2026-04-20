use std::pin::Pin;
use std::sync::Arc;

use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status};

use robot_fleet_proto::fleet::v1::{
    ota_service_server::OtaService, AckUpdateRequest, AckUpdateResponse, CheckUpdateRequest,
    CheckUpdateResponse, DispatchUpdateRequest, DispatchUpdateResponse, ListUpdateJobsRequest,
    ListUpdateJobsResponse, UpdateCommand, UpdateJob, WatchUpdatesRequest,
};

use crate::dispatcher::Dispatcher;
use crate::error::Error;
use crate::store::{JobStatus, NewJob, SqliteJobStore};

pub struct OtaServer {
    pub store: Arc<SqliteJobStore>,
    pub dispatcher: Dispatcher,
}

fn to_status(e: Error) -> Status {
    match e {
        Error::NotFound(msg) => Status::not_found(msg),
        Error::AlreadyPending => Status::already_exists("device already has an active job"),
        Error::NoActiveSubscribers(id) => {
            Status::unavailable(format!("device {id} is not connected"))
        }
        Error::Db(e) => Status::internal(e.to_string()),
        _ => Status::internal("internal error"),
    }
}

fn record_to_proto(r: &crate::store::JobRecord) -> UpdateJob {
    UpdateJob {
        update_job_id: r.id.clone(),
        device_id: r.device_id.clone(),
        target_version: r.target_version.clone(),
        artifact_url: r.artifact_url.clone(),
        status: r.status.as_str().to_string(),
        error_message: r.error_message.clone().unwrap_or_default(),
        created_at: None,
        completed_at: None,
    }
}

type BoxStream<T> = Pin<Box<dyn tokio_stream::Stream<Item = Result<T, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl OtaService for OtaServer {
    type WatchUpdatesStream = BoxStream<UpdateCommand>;

    async fn watch_updates(
        &self,
        request: Request<WatchUpdatesRequest>,
    ) -> Result<Response<Self::WatchUpdatesStream>, Status> {
        let device_id = request.into_inner().device_id;
        let rx = self.dispatcher.subscribe(&device_id).await;
        let stream = BroadcastStream::new(rx).filter_map(|res| {
            res.ok().map(Ok)
        });
        Ok(Response::new(Box::pin(stream)))
    }

    async fn check_update(
        &self,
        request: Request<CheckUpdateRequest>,
    ) -> Result<Response<CheckUpdateResponse>, Status> {
        let device_id = request.into_inner().device_id;
        let job = self
            .store
            .get_pending_for_device(&device_id)
            .map_err(to_status)?;

        let resp = match job {
            None => CheckUpdateResponse {
                update_available: false,
                ..Default::default()
            },
            Some(j) => CheckUpdateResponse {
                update_available: true,
                target_version: j.target_version,
                artifact_url: j.artifact_url,
                sha256: j.sha256,
                size_bytes: j.size_bytes,
                update_job_id: j.id,
            },
        };
        Ok(Response::new(resp))
    }

    async fn ack_update(
        &self,
        request: Request<AckUpdateRequest>,
    ) -> Result<Response<AckUpdateResponse>, Status> {
        let req = request.into_inner();
        let status = if req.success {
            JobStatus::Completed
        } else {
            JobStatus::Failed
        };
        let error = if req.error_message.is_empty() {
            None
        } else {
            Some(req.error_message.as_str())
        };
        self.store
            .update_status(&req.update_job_id, status, error)
            .map_err(to_status)?;
        Ok(Response::new(AckUpdateResponse { accepted: true }))
    }

    async fn dispatch_update(
        &self,
        request: Request<DispatchUpdateRequest>,
    ) -> Result<Response<DispatchUpdateResponse>, Status> {
        let req = request.into_inner();
        let job = self
            .store
            .create_job(NewJob {
                device_id: req.device_id.clone(),
                target_version: req.target_version.clone(),
                artifact_url: String::new(), // resolved by artifact-store in full impl
                sha256: String::new(),
                size_bytes: 0,
            })
            .map_err(to_status)?;

        let cmd = UpdateCommand {
            update_job_id: job.id.clone(),
            target_version: job.target_version.clone(),
            artifact_url: job.artifact_url.clone(),
            sha256: job.sha256.clone(),
            size_bytes: job.size_bytes,
            apply_by: req.apply_by,
        };

        self.dispatcher
            .dispatch(&req.device_id, cmd)
            .await
            .map_err(to_status)?;

        Ok(Response::new(DispatchUpdateResponse {
            job: Some(record_to_proto(&job)),
        }))
    }

    async fn list_update_jobs(
        &self,
        request: Request<ListUpdateJobsRequest>,
    ) -> Result<Response<ListUpdateJobsResponse>, Status> {
        let req = request.into_inner();
        let limit = if req.limit <= 0 { 20 } else { req.limit as i64 };
        let cursor = if req.cursor.is_empty() { None } else { Some(req.cursor.as_str()) };
        let (records, next_cursor) = self
            .store
            .list_for_device(&req.device_id, limit, cursor)
            .map_err(to_status)?;

        Ok(Response::new(ListUpdateJobsResponse {
            jobs: records.iter().map(record_to_proto).collect(),
            next_cursor: next_cursor.unwrap_or_default(),
        }))
    }
}
