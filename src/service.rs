//! Optional Axum service boundary for asynchronous simulation jobs.
//! Enable with the `service` Cargo feature.

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use axum::{
    Json, Router,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::{DefaultBodyLimit, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{Mutex, Semaphore, broadcast},
    task,
};

use crate::{
    CancellationToken, EngineDescriptor, ExecutionControl, ExecutionPolicy, SimulationError,
    SimulationRequest, SimulationResult, Simulator, VERSION,
};

pub const DEFAULT_MAX_RETAINED_JOBS: usize = 256;
pub const DEFAULT_MAX_CONCURRENT_JOBS: usize = 4;
pub const DEFAULT_MAX_SERVICE_REQUEST_BYTES: usize = 8 * 1024 * 1024;
const JOB_EVENT_CAPACITY: usize = 16;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceLimits {
    pub max_retained_jobs: usize,
    pub max_concurrent_jobs: usize,
    pub max_request_bytes: usize,
    pub execution_policy: ExecutionPolicy,
}

impl Default for ServiceLimits {
    fn default() -> Self {
        Self {
            max_retained_jobs: DEFAULT_MAX_RETAINED_JOBS,
            max_concurrent_jobs: DEFAULT_MAX_CONCURRENT_JOBS,
            max_request_bytes: DEFAULT_MAX_SERVICE_REQUEST_BYTES,
            execution_policy: ExecutionPolicy::default(),
        }
    }
}

impl ServiceLimits {
    pub fn validate(&self) -> Result<(), ServiceError> {
        if self.max_retained_jobs == 0 {
            return Err(ServiceError::new(
                "service_config_invalid",
                "max_retained_jobs must be greater than zero",
            ));
        }
        if self.max_concurrent_jobs == 0 || self.max_concurrent_jobs > self.max_retained_jobs {
            return Err(ServiceError::new(
                "service_config_invalid",
                "max_concurrent_jobs must be in 1..=max_retained_jobs",
            ));
        }
        if self.max_request_bytes == 0 {
            return Err(ServiceError::new(
                "service_config_invalid",
                "max_request_bytes must be greater than zero",
            ));
        }
        self.execution_policy.validate().map_err(|error| {
            ServiceError::new("service_config_invalid", error.message().to_owned())
        })?;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SimulationJobId(pub String);

impl SimulationJobId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SimulationJobStatus {
    Queued,
    Running,
    Cancelling,
    Succeeded,
    Failed,
    Cancelled,
}

impl SimulationJobStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimulationJobFailure {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simulation: Option<SimulationError>,
}

impl SimulationJobFailure {
    fn simulation(error: SimulationError) -> Self {
        Self {
            code: error.code().to_owned(),
            message: error.to_string(),
            simulation: Some(error),
        }
    }

    fn worker(message: impl Into<String>) -> Self {
        Self {
            code: "service_worker_failed".to_owned(),
            message: message.into(),
            simulation: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimulationJobSnapshot {
    pub id: SimulationJobId,
    pub status: SimulationJobStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<SimulationResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<SimulationJobFailure>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimulationJobEvent {
    pub job: SimulationJobSnapshot,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SubmitSimulation {
    pub request: SimulationRequest,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceError {
    pub code: String,
    pub message: String,
}

impl ServiceError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    fn status(&self) -> StatusCode {
        match self.code.as_str() {
            "simulation_job_not_found" => StatusCode::NOT_FOUND,
            "service_queue_full" => StatusCode::TOO_MANY_REQUESTS,
            "service_config_invalid" => StatusCode::INTERNAL_SERVER_ERROR,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ServiceError {}

impl IntoResponse for ServiceError {
    fn into_response(self) -> Response {
        (self.status(), Json(self)).into_response()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
}

#[derive(Clone)]
struct JobEntry {
    snapshot: SimulationJobSnapshot,
    cancellation: CancellationToken,
    events: broadcast::Sender<SimulationJobEvent>,
}

#[derive(Clone)]
struct ServiceState {
    simulator: Arc<Simulator>,
    limits: ServiceLimits,
    jobs: Arc<Mutex<BTreeMap<SimulationJobId, JobEntry>>>,
    gate: Arc<Semaphore>,
    next_job: Arc<AtomicU64>,
}

#[derive(Clone)]
pub struct SimulationService {
    state: ServiceState,
}

impl SimulationService {
    pub fn new(simulator: Simulator, limits: ServiceLimits) -> Result<Self, ServiceError> {
        limits.validate()?;
        Ok(Self {
            state: ServiceState {
                simulator: Arc::new(simulator),
                gate: Arc::new(Semaphore::new(limits.max_concurrent_jobs)),
                limits,
                jobs: Arc::new(Mutex::new(BTreeMap::new())),
                next_job: Arc::new(AtomicU64::new(1)),
            },
        })
    }

    pub fn limits(&self) -> &ServiceLimits {
        &self.state.limits
    }

    pub fn engine_descriptors(&self) -> Vec<EngineDescriptor> {
        self.state.simulator.engine_descriptors()
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route("/healthz", get(health))
            .route("/v1/engines", get(engines))
            .route("/v1/simulations", post(submit_simulation))
            .route(
                "/v1/simulations/{id}",
                get(get_simulation).delete(cancel_simulation),
            )
            .route("/v1/simulations/{id}/events", any(simulation_events))
            .layer(DefaultBodyLimit::max(self.state.limits.max_request_bytes))
            .with_state(self.state.clone())
    }

    pub async fn submit(
        &self,
        submission: SubmitSimulation,
    ) -> Result<SimulationJobSnapshot, ServiceError> {
        submit_job(&self.state, submission).await
    }

    pub async fn get(&self, id: &SimulationJobId) -> Result<SimulationJobSnapshot, ServiceError> {
        snapshot_job(&self.state, id).await
    }

    pub async fn cancel(
        &self,
        id: &SimulationJobId,
    ) -> Result<SimulationJobSnapshot, ServiceError> {
        cancel_job(&self.state, id).await
    }

    pub async fn subscribe(
        &self,
        id: &SimulationJobId,
    ) -> Result<
        (
            SimulationJobSnapshot,
            broadcast::Receiver<SimulationJobEvent>,
        ),
        ServiceError,
    > {
        subscribe_job(&self.state, id).await
    }
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: VERSION,
    })
}

async fn engines(State(state): State<ServiceState>) -> Json<Vec<EngineDescriptor>> {
    Json(state.simulator.engine_descriptors())
}

async fn submit_simulation(
    State(state): State<ServiceState>,
    Json(submission): Json<SubmitSimulation>,
) -> Result<impl IntoResponse, ServiceError> {
    let snapshot = submit_job(&state, submission).await?;
    Ok((StatusCode::ACCEPTED, Json(snapshot)))
}

async fn get_simulation(
    State(state): State<ServiceState>,
    Path(id): Path<String>,
) -> Result<Json<SimulationJobSnapshot>, ServiceError> {
    snapshot_job(&state, &SimulationJobId(id)).await.map(Json)
}

async fn cancel_simulation(
    State(state): State<ServiceState>,
    Path(id): Path<String>,
) -> Result<Json<SimulationJobSnapshot>, ServiceError> {
    cancel_job(&state, &SimulationJobId(id)).await.map(Json)
}

async fn simulation_events(
    ws: WebSocketUpgrade,
    State(state): State<ServiceState>,
    Path(id): Path<String>,
) -> Result<Response, ServiceError> {
    let (snapshot, receiver) = subscribe_job(&state, &SimulationJobId(id)).await?;
    Ok(ws.on_upgrade(move |socket| stream_job(socket, snapshot, receiver)))
}

async fn stream_job(
    mut socket: WebSocket,
    snapshot: SimulationJobSnapshot,
    mut receiver: broadcast::Receiver<SimulationJobEvent>,
) {
    if send_event(
        &mut socket,
        SimulationJobEvent {
            job: snapshot.clone(),
        },
    )
    .await
    .is_err()
    {
        return;
    }
    if snapshot.status.is_terminal() {
        return;
    }

    loop {
        match receiver.recv().await {
            Ok(event) => {
                let terminal = event.job.status.is_terminal();
                if send_event(&mut socket, event).await.is_err() || terminal {
                    return;
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

async fn send_event(socket: &mut WebSocket, event: SimulationJobEvent) -> Result<(), ()> {
    let payload = serde_json::to_string(&event).map_err(|_| ())?;
    socket
        .send(Message::Text(payload.into()))
        .await
        .map_err(|_| ())
}

async fn submit_job(
    state: &ServiceState,
    submission: SubmitSimulation,
) -> Result<SimulationJobSnapshot, ServiceError> {
    let id = SimulationJobId(format!(
        "sim-{:016x}",
        state.next_job.fetch_add(1, Ordering::Relaxed)
    ));
    let cancellation = CancellationToken::new();
    let (events, _) = broadcast::channel(JOB_EVENT_CAPACITY);
    let snapshot = SimulationJobSnapshot {
        id: id.clone(),
        status: SimulationJobStatus::Queued,
        result: None,
        error: None,
    };

    {
        let mut jobs = state.jobs.lock().await;
        prune_terminal_jobs(&mut jobs, state.limits.max_retained_jobs);
        if jobs.len() >= state.limits.max_retained_jobs {
            return Err(ServiceError::new(
                "service_queue_full",
                "all retained service job slots are occupied by active work",
            ));
        }
        jobs.insert(
            id.clone(),
            JobEntry {
                snapshot: snapshot.clone(),
                cancellation: cancellation.clone(),
                events,
            },
        );
    }

    let state = state.clone();
    tokio::spawn(async move {
        run_job(state, id, submission.request, cancellation).await;
    });

    Ok(snapshot)
}

fn prune_terminal_jobs(jobs: &mut BTreeMap<SimulationJobId, JobEntry>, max_retained: usize) {
    if jobs.len() < max_retained {
        return;
    }
    let removable: Vec<_> = jobs
        .iter()
        .filter(|(_, entry)| entry.snapshot.status.is_terminal())
        .map(|(id, _)| id.clone())
        .collect();
    for id in removable {
        if jobs.len() < max_retained {
            break;
        }
        jobs.remove(&id);
    }
}

async fn run_job(
    state: ServiceState,
    id: SimulationJobId,
    request: SimulationRequest,
    cancellation: CancellationToken,
) {
    let permit = match state.gate.clone().acquire_owned().await {
        Ok(permit) => permit,
        Err(_) => {
            finish_worker_failure(&state, &id, "service worker gate was closed").await;
            return;
        }
    };

    if cancellation.is_cancelled() {
        finish_cancelled(&state, &id).await;
        drop(permit);
        return;
    }

    update_status(&state, &id, SimulationJobStatus::Running).await;
    let simulator = state.simulator.clone();
    let policy = state.limits.execution_policy.clone();
    let worker_cancellation = cancellation.clone();
    let outcome = task::spawn_blocking(move || {
        simulator.simulate_with_control(
            &request,
            &ExecutionControl::new(policy).with_cancellation(worker_cancellation),
        )
    })
    .await;
    drop(permit);

    if cancellation.is_cancelled() {
        finish_cancelled(&state, &id).await;
        return;
    }

    match outcome {
        Ok(Ok(result)) => finish_success(&state, &id, result).await,
        Ok(Err(error)) if simulation_was_cancelled(&error) => finish_cancelled(&state, &id).await,
        Ok(Err(error)) => finish_simulation_failure(&state, &id, error).await,
        Err(error) => {
            finish_worker_failure(&state, &id, format!("simulation worker failed: {error}")).await
        }
    }
}

fn simulation_was_cancelled(error: &SimulationError) -> bool {
    matches!(
        error,
        SimulationError::Engine { error, .. } if error.is_cancelled()
    )
}

async fn snapshot_job(
    state: &ServiceState,
    id: &SimulationJobId,
) -> Result<SimulationJobSnapshot, ServiceError> {
    let jobs = state.jobs.lock().await;
    jobs.get(id)
        .map(|entry| entry.snapshot.clone())
        .ok_or_else(job_not_found)
}

async fn subscribe_job(
    state: &ServiceState,
    id: &SimulationJobId,
) -> Result<
    (
        SimulationJobSnapshot,
        broadcast::Receiver<SimulationJobEvent>,
    ),
    ServiceError,
> {
    let jobs = state.jobs.lock().await;
    let entry = jobs.get(id).ok_or_else(job_not_found)?;
    Ok((entry.snapshot.clone(), entry.events.subscribe()))
}

async fn cancel_job(
    state: &ServiceState,
    id: &SimulationJobId,
) -> Result<SimulationJobSnapshot, ServiceError> {
    let mut jobs = state.jobs.lock().await;
    let entry = jobs.get_mut(id).ok_or_else(job_not_found)?;
    if !entry.snapshot.status.is_terminal() {
        entry.cancellation.cancel();
        entry.snapshot.status = SimulationJobStatus::Cancelling;
        publish(entry);
    }
    Ok(entry.snapshot.clone())
}

async fn update_status(state: &ServiceState, id: &SimulationJobId, status: SimulationJobStatus) {
    let mut jobs = state.jobs.lock().await;
    if let Some(entry) = jobs.get_mut(id) {
        entry.snapshot.status = status;
        publish(entry);
    }
}

async fn finish_success(state: &ServiceState, id: &SimulationJobId, result: SimulationResult) {
    let mut jobs = state.jobs.lock().await;
    if let Some(entry) = jobs.get_mut(id) {
        if entry.cancellation.is_cancelled() {
            entry.snapshot.status = SimulationJobStatus::Cancelled;
            entry.snapshot.result = None;
            entry.snapshot.error = None;
        } else {
            entry.snapshot.status = SimulationJobStatus::Succeeded;
            entry.snapshot.result = Some(result);
            entry.snapshot.error = None;
        }
        publish(entry);
    }
}

async fn finish_simulation_failure(
    state: &ServiceState,
    id: &SimulationJobId,
    error: SimulationError,
) {
    let mut jobs = state.jobs.lock().await;
    if let Some(entry) = jobs.get_mut(id) {
        if entry.cancellation.is_cancelled() {
            entry.snapshot.status = SimulationJobStatus::Cancelled;
            entry.snapshot.result = None;
            entry.snapshot.error = None;
        } else {
            entry.snapshot.status = SimulationJobStatus::Failed;
            entry.snapshot.result = None;
            entry.snapshot.error = Some(SimulationJobFailure::simulation(error));
        }
        publish(entry);
    }
}

async fn finish_worker_failure(
    state: &ServiceState,
    id: &SimulationJobId,
    message: impl Into<String>,
) {
    let mut jobs = state.jobs.lock().await;
    if let Some(entry) = jobs.get_mut(id) {
        entry.snapshot.status = SimulationJobStatus::Failed;
        entry.snapshot.result = None;
        entry.snapshot.error = Some(SimulationJobFailure::worker(message));
        publish(entry);
    }
}

async fn finish_cancelled(state: &ServiceState, id: &SimulationJobId) {
    let mut jobs = state.jobs.lock().await;
    if let Some(entry) = jobs.get_mut(id) {
        entry.snapshot.status = SimulationJobStatus::Cancelled;
        entry.snapshot.result = None;
        entry.snapshot.error = None;
        publish(entry);
    }
}

fn publish(entry: &JobEntry) {
    let _ = entry.events.send(SimulationJobEvent {
        job: entry.snapshot.clone(),
    });
}

fn job_not_found() -> ServiceError {
    ServiceError::new("simulation_job_not_found", "simulation job was not found")
}
