//! Optional production runtime for durable, parallel, reproducible simulation jobs.
//!
//! Enable with the `production` Cargo feature. The runtime deliberately uses
//! the filesystem as its durable queue and content store: atomic same-filesystem
//! renames provide claim/recovery semantics without embedding a second server.
//! Linux hard isolation is delegated to bubblewrap + prlimit through
//! `IsolatedProcessExecutor`.

use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ngspice::ChildGuard;
use crate::{
    CancellationToken, DigitalEngine, EngineDescriptor, EngineError, EngineId, ExecutionControl,
    ExecutionPolicy, MixedSignalEngine, NgSpiceEngine, SimulationError, SimulationRequest,
    SimulationResult, Simulator, VERSION, VerilatorEngine, XSpiceEngine,
};

pub const RUNTIME_MANIFEST_SCHEMA_VERSION: u16 = 1;
pub const DEFAULT_MAX_PARALLEL_WORKERS: usize = 4;
pub const DEFAULT_MAX_QUEUED_JOBS: usize = 512;
pub const DEFAULT_MAX_RETAINED_TERMINAL_JOBS: usize = 1024;
pub const DEFAULT_MAX_ATTEMPTS: u32 = 2;
pub const DEFAULT_WORKER_MEMORY_BYTES: u64 = 1024 * 1024 * 1024;
pub const DEFAULT_WORKER_CPU_SECONDS: u64 = 120;
pub const DEFAULT_WORKER_PROCESSES: u64 = 128;
pub const DEFAULT_WORKER_OPEN_FILES: u64 = 256;
pub const DEFAULT_WORKER_FILE_BYTES: u64 = 256 * 1024 * 1024;

static UNIQUE_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProductionRuntimeLimits {
    pub max_parallel_workers: usize,
    pub max_queued_jobs: usize,
    pub max_retained_terminal_jobs: usize,
    pub max_attempts: u32,
    pub execution_policy: ExecutionPolicy,
}

impl Default for ProductionRuntimeLimits {
    fn default() -> Self {
        Self {
            max_parallel_workers: DEFAULT_MAX_PARALLEL_WORKERS,
            max_queued_jobs: DEFAULT_MAX_QUEUED_JOBS,
            max_retained_terminal_jobs: DEFAULT_MAX_RETAINED_TERMINAL_JOBS,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            execution_policy: ExecutionPolicy::default(),
        }
    }
}

impl ProductionRuntimeLimits {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.max_parallel_workers == 0 {
            return Err(RuntimeError::new(
                "runtime_config_invalid",
                "max_parallel_workers must be greater than zero",
            ));
        }
        if self.max_queued_jobs == 0 {
            return Err(RuntimeError::new(
                "runtime_config_invalid",
                "max_queued_jobs must be greater than zero",
            ));
        }
        if self.max_retained_terminal_jobs == 0 {
            return Err(RuntimeError::new(
                "runtime_config_invalid",
                "max_retained_terminal_jobs must be greater than zero",
            ));
        }
        if self.max_attempts == 0 {
            return Err(RuntimeError::new(
                "runtime_config_invalid",
                "max_attempts must be greater than zero",
            ));
        }
        self.execution_policy.validate().map_err(|error| {
            RuntimeError::new("runtime_config_invalid", error.message().to_owned())
        })?;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeError {
    pub code: String,
    pub message: String,
}

impl RuntimeError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for RuntimeError {}

impl From<io::Error> for RuntimeError {
    fn from(error: io::Error) -> Self {
        Self::new("runtime_io_failed", error.to_string())
    }
}

impl From<serde_json::Error> for RuntimeError {
    fn from(error: serde_json::Error) -> Self {
        Self::new("runtime_serialization_failed", error.to_string())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RuntimeJobId(pub String);

impl RuntimeJobId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeJobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl RuntimeJobStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReproducibilityManifest {
    pub schema_version: u16,
    pub core_version: String,
    pub request_sha256: String,
    pub cache_key_sha256: String,
    pub executor_fingerprint: String,
    pub engine_descriptors: Vec<EngineDescriptor>,
    pub execution_policy: ExecutionPolicy,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RuntimeJobFailure {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simulation: Option<SimulationError>,
}

impl RuntimeJobFailure {
    fn simulation(error: SimulationError) -> Self {
        Self {
            code: error.code().to_owned(),
            message: error.to_string(),
            simulation: Some(error),
        }
    }

    fn infrastructure(error: RuntimeError) -> Self {
        Self {
            code: error.code,
            message: error.message,
            simulation: None,
        }
    }

    fn retryable(&self) -> bool {
        matches!(
            &self.simulation,
            Some(SimulationError::Engine { error, .. }) if error.is_retryable()
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RuntimeJobSnapshot {
    pub id: RuntimeJobId,
    pub status: RuntimeJobStatus,
    pub attempts: u32,
    pub cache_hit: bool,
    pub manifest: ReproducibilityManifest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<SimulationResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RuntimeJobFailure>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredJob {
    snapshot: RuntimeJobSnapshot,
    request: SimulationRequest,
}

pub trait RuntimeExecutor: Send + Sync {
    fn engine_descriptors(&self) -> Vec<EngineDescriptor>;
    fn fingerprint(&self) -> String;
    fn execute(
        &self,
        request: &SimulationRequest,
        control: &ExecutionControl,
    ) -> Result<SimulationResult, SimulationError>;
}

#[derive(Clone)]
pub struct SimulatorExecutor {
    simulator: Arc<Simulator>,
    fingerprint: String,
}

impl SimulatorExecutor {
    pub fn new(simulator: Simulator) -> Result<Self, RuntimeError> {
        let mut descriptors = simulator.engine_descriptors();
        descriptors.sort_by(|left, right| left.id.cmp(&right.id));
        let fingerprint = format!(
            "simulator:{}",
            sha256_hex(&serde_json::to_vec(&(VERSION, &descriptors))?)
        );
        Ok(Self {
            simulator: Arc::new(simulator),
            fingerprint,
        })
    }
}

impl RuntimeExecutor for SimulatorExecutor {
    fn engine_descriptors(&self) -> Vec<EngineDescriptor> {
        let mut descriptors = self.simulator.engine_descriptors();
        descriptors.sort_by(|left, right| left.id.cmp(&right.id));
        descriptors
    }

    fn fingerprint(&self) -> String {
        self.fingerprint.clone()
    }

    fn execute(
        &self,
        request: &SimulationRequest,
        control: &ExecutionControl,
    ) -> Result<SimulationResult, SimulationError> {
        self.simulator.simulate_with_control(request, control)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IsolatedProcessLimits {
    pub memory_bytes: u64,
    pub cpu_seconds: u64,
    pub max_processes: u64,
    pub max_open_files: u64,
    pub max_file_bytes: u64,
}

impl Default for IsolatedProcessLimits {
    fn default() -> Self {
        Self {
            memory_bytes: DEFAULT_WORKER_MEMORY_BYTES,
            cpu_seconds: DEFAULT_WORKER_CPU_SECONDS,
            max_processes: DEFAULT_WORKER_PROCESSES,
            max_open_files: DEFAULT_WORKER_OPEN_FILES,
            max_file_bytes: DEFAULT_WORKER_FILE_BYTES,
        }
    }
}

impl IsolatedProcessLimits {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.memory_bytes == 0
            || self.cpu_seconds == 0
            || self.max_processes == 0
            || self.max_open_files == 0
            || self.max_file_bytes == 0
        {
            return Err(RuntimeError::new(
                "runtime_isolation_invalid",
                "all hard worker limits must be greater than zero",
            ));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct IsolatedProcessExecutor {
    worker: PathBuf,
    descriptors: Vec<EngineDescriptor>,
    fingerprint: String,
    limits: IsolatedProcessLimits,
    bubblewrap: PathBuf,
    prlimit: PathBuf,
}

impl IsolatedProcessExecutor {
    pub fn new(
        worker: impl AsRef<Path>,
        descriptors: Vec<EngineDescriptor>,
        toolchain_fingerprint: impl Into<String>,
        limits: IsolatedProcessLimits,
    ) -> Result<Self, RuntimeError> {
        limits.validate()?;
        let worker = fs::canonicalize(worker.as_ref()).map_err(|error| {
            RuntimeError::new(
                "runtime_worker_unavailable",
                format!("could not resolve isolated worker executable: {error}"),
            )
        })?;
        if !worker.is_file() {
            return Err(RuntimeError::new(
                "runtime_worker_unavailable",
                "isolated worker path is not a file",
            ));
        }
        let bubblewrap = find_executable("bwrap").ok_or_else(|| {
            RuntimeError::new(
                "runtime_isolation_unavailable",
                "bubblewrap (`bwrap`) is required for production worker isolation",
            )
        })?;
        let prlimit = find_executable("prlimit").ok_or_else(|| {
            RuntimeError::new(
                "runtime_isolation_unavailable",
                "`prlimit` is required for production worker resource limits",
            )
        })?;
        let toolchain_fingerprint = toolchain_fingerprint.into();
        if toolchain_fingerprint.trim().is_empty() {
            return Err(RuntimeError::new(
                "runtime_isolation_invalid",
                "toolchain_fingerprint must identify the immutable worker/toolchain image",
            ));
        }
        let mut descriptors = descriptors;
        descriptors.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(Self {
            worker,
            descriptors,
            fingerprint: format!("isolated:{toolchain_fingerprint}"),
            limits,
            bubblewrap,
            prlimit,
        })
    }

    pub fn isolation_command_preview(&self) -> Vec<String> {
        let run_dir = Path::new("/runtime-job");
        self.command_arguments(run_dir)
    }

    fn command_arguments(&self, run_dir: &Path) -> Vec<String> {
        let mut args = vec![
            "--die-with-parent".to_owned(),
            "--new-session".to_owned(),
            "--unshare-user".to_owned(),
            "--unshare-pid".to_owned(),
            "--unshare-ipc".to_owned(),
            "--unshare-uts".to_owned(),
            "--unshare-net".to_owned(),
            "--ro-bind".to_owned(),
            "/usr".to_owned(),
            "/usr".to_owned(),
        ];
        for system_path in ["/bin", "/sbin", "/lib", "/lib64", "/etc", "/opt"] {
            if Path::new(system_path).exists() {
                args.push("--ro-bind".to_owned());
                args.push(system_path.to_owned());
                args.push(system_path.to_owned());
            }
        }
        args.extend([
            "--proc".to_owned(),
            "/proc".to_owned(),
            "--dev".to_owned(),
            "/dev".to_owned(),
            "--tmpfs".to_owned(),
            "/tmp".to_owned(),
            "--dir".to_owned(),
            "/worker".to_owned(),
            "--ro-bind".to_owned(),
            self.worker.display().to_string(),
            "/worker/sim-worker".to_owned(),
            "--bind".to_owned(),
            run_dir.display().to_string(),
            "/work".to_owned(),
            "--chdir".to_owned(),
            "/work".to_owned(),
            "--clearenv".to_owned(),
            "--setenv".to_owned(),
            "HOME".to_owned(),
            "/tmp".to_owned(),
            "--setenv".to_owned(),
            "TMPDIR".to_owned(),
            "/tmp".to_owned(),
            "--setenv".to_owned(),
            "PATH".to_owned(),
            "/usr/bin:/bin".to_owned(),
            self.prlimit.display().to_string(),
            format!("--as={}", self.limits.memory_bytes),
            format!("--cpu={}", self.limits.cpu_seconds),
            format!("--nproc={}", self.limits.max_processes),
            format!("--nofile={}", self.limits.max_open_files),
            format!("--fsize={}", self.limits.max_file_bytes),
            "--".to_owned(),
            "/worker/sim-worker".to_owned(),
            "--request".to_owned(),
            "/work/request.json".to_owned(),
            "--response".to_owned(),
            "/work/response.json".to_owned(),
        ]);
        args
    }
}

impl RuntimeExecutor for IsolatedProcessExecutor {
    fn engine_descriptors(&self) -> Vec<EngineDescriptor> {
        self.descriptors.clone()
    }

    fn fingerprint(&self) -> String {
        self.fingerprint.clone()
    }

    fn execute(
        &self,
        request: &SimulationRequest,
        control: &ExecutionControl,
    ) -> Result<SimulationResult, SimulationError> {
        match self.execute_inner(request, control) {
            Ok(result) => result,
            Err(error) => Err(worker_simulation_error(error)),
        }
    }
}

impl IsolatedProcessExecutor {
    fn execute_inner(
        &self,
        request: &SimulationRequest,
        control: &ExecutionControl,
    ) -> Result<Result<SimulationResult, SimulationError>, RuntimeError> {
        control.policy.validate().map_err(|error| {
            RuntimeError::new(
                "runtime_execution_policy_invalid",
                error.message().to_owned(),
            )
        })?;
        if control.cancellation.is_cancelled() {
            return Ok(Err(worker_engine_error(
                "execution_cancelled",
                "isolated worker was cancelled before execution",
                false,
            )));
        }

        let run_dir = unique_temp_dir("ontologyx-sim-worker")?;
        let outcome = self.execute_in_dir(request, control, &run_dir);
        let _ = fs::remove_dir_all(&run_dir);
        outcome
    }

    fn execute_in_dir(
        &self,
        request: &SimulationRequest,
        control: &ExecutionControl,
        run_dir: &Path,
    ) -> Result<Result<SimulationResult, SimulationError>, RuntimeError> {
        let worker_request = WorkerRequestEnvelope {
            request: request.clone(),
            execution_policy: control.policy.clone(),
        };
        let request_bytes = serde_json::to_vec(&worker_request)?;
        enforce_bytes(
            "isolated worker input",
            request_bytes.len() as u64,
            control.policy.max_input_bytes,
        )?;
        fs::write(run_dir.join("request.json"), request_bytes)?;

        let log = File::create(run_dir.join("worker.log"))?;
        let log_err = log.try_clone()?;
        let mut command = Command::new(&self.bubblewrap);
        command
            .args(self.command_arguments(run_dir))
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err));
        let child = command.spawn().map_err(|error| {
            RuntimeError::new(
                "runtime_isolation_failed",
                format!("could not start bubblewrap worker: {error}"),
            )
        })?;
        let mut child = ChildGuard::new(child);
        let started = Instant::now();
        loop {
            match child.child_mut().try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        return Err(RuntimeError::new(
                            "runtime_worker_failed",
                            format!(
                                "isolated worker exited with {status}: {}",
                                bounded_file(
                                    &run_dir.join("worker.log"),
                                    control.policy.max_log_bytes
                                )
                            ),
                        ));
                    }
                    child.disarm();
                    break;
                }
                Ok(None) => {}
                Err(error) => {
                    return Err(RuntimeError::new(
                        "runtime_worker_failed",
                        format!("could not query isolated worker status: {error}"),
                    ));
                }
            }
            if control.cancellation.is_cancelled() {
                let _ = child.child_mut().kill();
                let _ = child.child_mut().wait();
                child.disarm();
                return Ok(Err(worker_engine_error(
                    "execution_cancelled",
                    "isolated worker execution was cancelled",
                    false,
                )));
            }
            if control.policy.timeout_ms != 0
                && started.elapsed() >= Duration::from_millis(control.policy.timeout_ms)
            {
                let _ = child.child_mut().kill();
                let _ = child.child_mut().wait();
                child.disarm();
                return Ok(Err(worker_engine_error(
                    "execution_timeout",
                    format!(
                        "isolated worker exceeded timeout of {} ms",
                        control.policy.timeout_ms
                    ),
                    true,
                )));
            }
            if let Ok(metadata) = fs::metadata(run_dir.join("worker.log")) {
                enforce_bytes(
                    "isolated worker log",
                    metadata.len(),
                    control.policy.max_log_bytes,
                )?;
            }
            thread::sleep(Duration::from_millis(control.policy.poll_interval_ms));
        }

        let response_path = run_dir.join("response.json");
        let response_bytes = read_bounded(
            &response_path,
            control.policy.max_output_bytes,
            "isolated worker response",
        )?;
        let response: WorkerResponseEnvelope = serde_json::from_slice(&response_bytes)?;
        match (response.result, response.error) {
            (Some(result), None) => Ok(Ok(result)),
            (None, Some(error)) => Ok(Err(error)),
            _ => Err(RuntimeError::new(
                "runtime_worker_protocol_invalid",
                "worker response must contain exactly one of result or error",
            )),
        }
    }
}

#[derive(Clone)]
pub struct ProductionRuntime {
    root: Arc<PathBuf>,
    executor: Arc<dyn RuntimeExecutor>,
    limits: ProductionRuntimeLimits,
    active: Arc<Mutex<BTreeMap<RuntimeJobId, CancellationToken>>>,
}

impl ProductionRuntime {
    pub fn open(
        root: impl Into<PathBuf>,
        executor: Arc<dyn RuntimeExecutor>,
        limits: ProductionRuntimeLimits,
    ) -> Result<Self, RuntimeError> {
        limits.validate()?;
        let runtime = Self {
            root: Arc::new(root.into()),
            executor,
            limits,
            active: Arc::new(Mutex::new(BTreeMap::new())),
        };
        runtime.ensure_layout()?;
        runtime.recover_incomplete()?;
        runtime.prune_terminal()?;
        Ok(runtime)
    }

    pub fn limits(&self) -> &ProductionRuntimeLimits {
        &self.limits
    }

    pub fn root(&self) -> &Path {
        self.root.as_path()
    }

    pub fn content_store(&self) -> ContentAddressedStore {
        ContentAddressedStore::new(self.root.join("artifacts/sha256"))
    }

    pub fn submit(&self, request: SimulationRequest) -> Result<RuntimeJobSnapshot, RuntimeError> {
        if count_json_files(&self.queue_dir())? >= self.limits.max_queued_jobs {
            return Err(RuntimeError::new(
                "runtime_queue_full",
                "durable simulation queue is full",
            ));
        }
        let manifest = build_manifest(
            &request,
            &self.limits.execution_policy,
            self.executor.as_ref(),
        )?;
        if let Some(result) = self.read_cache(&manifest.cache_key_sha256)? {
            let snapshot = RuntimeJobSnapshot {
                id: new_job_id(&manifest.cache_key_sha256),
                status: RuntimeJobStatus::Succeeded,
                attempts: 0,
                cache_hit: true,
                manifest,
                result: Some(result),
                error: None,
            };
            let stored = StoredJob {
                snapshot: snapshot.clone(),
                request,
            };
            write_json_atomic(&self.terminal_path(&snapshot.id), &stored)?;
            self.prune_terminal()?;
            return Ok(snapshot);
        }

        let id = new_job_id(&manifest.cache_key_sha256);
        let snapshot = RuntimeJobSnapshot {
            id: id.clone(),
            status: RuntimeJobStatus::Queued,
            attempts: 0,
            cache_hit: false,
            manifest,
            result: None,
            error: None,
        };
        write_json_atomic(
            &self.queue_path(&id),
            &StoredJob {
                snapshot: snapshot.clone(),
                request,
            },
        )?;
        Ok(snapshot)
    }

    pub fn get(&self, id: &RuntimeJobId) -> Result<RuntimeJobSnapshot, RuntimeError> {
        for path in [
            self.queue_path(id),
            self.running_path(id),
            self.terminal_path(id),
        ] {
            if path.is_file() {
                return Ok(read_job(&path)?.snapshot);
            }
        }
        Err(RuntimeError::new(
            "runtime_job_not_found",
            format!("runtime job `{}` was not found", id.as_str()),
        ))
    }

    pub fn cancel(&self, id: &RuntimeJobId) -> Result<RuntimeJobSnapshot, RuntimeError> {
        let queued = self.queue_path(id);
        let terminal = self.terminal_path(id);
        if queued.is_file() {
            match fs::rename(&queued, &terminal) {
                Ok(()) => {
                    let mut job = read_job(&terminal)?;
                    job.snapshot.status = RuntimeJobStatus::Cancelled;
                    job.snapshot.result = None;
                    job.snapshot.error = None;
                    write_json_atomic(&terminal, &job)?;
                    return Ok(job.snapshot);
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }

        // Persist a cancellation marker as well as signalling the in-memory
        // token. The marker closes the small claim->active-registration race.
        fs::write(self.cancel_path(id), b"cancelled\n")?;
        if let Some(token) = self
            .active
            .lock()
            .map_err(|_| {
                RuntimeError::new("runtime_state_poisoned", "active worker state poisoned")
            })?
            .get(id)
            .cloned()
        {
            token.cancel();
        }

        let snapshot = self.get(id)?;
        if snapshot.status.is_terminal() {
            let _ = fs::remove_file(self.cancel_path(id));
        }
        Ok(snapshot)
    }

    pub fn recover_incomplete(&self) -> Result<usize, RuntimeError> {
        let mut recovered = 0;
        for path in json_files(&self.running_dir())? {
            let mut job = read_job(&path)?;
            job.snapshot.status = RuntimeJobStatus::Queued;
            job.snapshot.result = None;
            write_json_atomic(&path, &job)?;
            let destination = self.queue_path(&job.snapshot.id);
            if destination.exists() {
                return Err(RuntimeError::new(
                    "runtime_recovery_conflict",
                    format!(
                        "both queued and running state exist for `{}`",
                        job.snapshot.id.as_str()
                    ),
                ));
            }
            fs::rename(&path, destination)?;
            recovered += 1;
        }
        Ok(recovered)
    }

    pub async fn drain(&self) -> Result<usize, RuntimeError> {
        let mut handles = Vec::with_capacity(self.limits.max_parallel_workers);
        for _ in 0..self.limits.max_parallel_workers {
            let runtime = self.clone();
            handles.push(tokio::spawn(async move {
                let mut completed = 0usize;
                while runtime.run_next().await? {
                    completed += 1;
                }
                Ok::<usize, RuntimeError>(completed)
            }));
        }
        let mut total = 0;
        for handle in handles {
            total += handle.await.map_err(|error| {
                RuntimeError::new(
                    "runtime_worker_failed",
                    format!("worker task failed: {error}"),
                )
            })??;
        }
        self.prune_terminal()?;
        Ok(total)
    }

    pub async fn run_next(&self) -> Result<bool, RuntimeError> {
        let Some(mut job) = self.claim_next()? else {
            return Ok(false);
        };

        if let Some(result) = self.read_cache(&job.snapshot.manifest.cache_key_sha256)? {
            job.snapshot.status = RuntimeJobStatus::Succeeded;
            job.snapshot.cache_hit = true;
            job.snapshot.result = Some(result);
            job.snapshot.error = None;
            self.finish_job(job)?;
            return Ok(true);
        }

        let cancellation = CancellationToken::new();
        self.active
            .lock()
            .map_err(|_| {
                RuntimeError::new("runtime_state_poisoned", "active worker state poisoned")
            })?
            .insert(job.snapshot.id.clone(), cancellation.clone());
        if self.cancel_path(&job.snapshot.id).is_file() {
            cancellation.cancel();
        }

        let executor = self.executor.clone();
        let request = job.request.clone();
        let policy = self.limits.execution_policy.clone();
        let worker_cancel = cancellation.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            executor.execute(
                &request,
                &ExecutionControl::new(policy).with_cancellation(worker_cancel),
            )
        })
        .await
        .map_err(|error| {
            RuntimeError::new(
                "runtime_worker_failed",
                format!("worker task failed: {error}"),
            )
        });

        self.active
            .lock()
            .map_err(|_| {
                RuntimeError::new("runtime_state_poisoned", "active worker state poisoned")
            })?
            .remove(&job.snapshot.id);
        if self.cancel_path(&job.snapshot.id).is_file() {
            cancellation.cancel();
        }

        if cancellation.is_cancelled() {
            job.snapshot.status = RuntimeJobStatus::Cancelled;
            job.snapshot.result = None;
            job.snapshot.error = None;
            let id = job.snapshot.id.clone();
            self.finish_job(job)?;
            let _ = fs::remove_file(self.cancel_path(&id));
            return Ok(true);
        }

        match outcome {
            Ok(Ok(result)) => {
                self.write_cache(&job.snapshot.manifest.cache_key_sha256, &result)?;
                job.snapshot.status = RuntimeJobStatus::Succeeded;
                job.snapshot.result = Some(result);
                job.snapshot.error = None;
                self.finish_job(job)?;
            }
            Ok(Err(error)) => {
                let failure = RuntimeJobFailure::simulation(error);
                if failure.retryable() && job.snapshot.attempts < self.limits.max_attempts {
                    job.snapshot.status = RuntimeJobStatus::Queued;
                    job.snapshot.error = Some(failure);
                    self.requeue_job(job)?;
                } else {
                    job.snapshot.status = if matches!(
                        &failure.simulation,
                        Some(SimulationError::Engine { error, .. }) if error.is_cancelled()
                    ) {
                        RuntimeJobStatus::Cancelled
                    } else {
                        RuntimeJobStatus::Failed
                    };
                    job.snapshot.error = if job.snapshot.status == RuntimeJobStatus::Cancelled {
                        None
                    } else {
                        Some(failure)
                    };
                    self.finish_job(job)?;
                }
            }
            Err(error) => {
                job.snapshot.status = RuntimeJobStatus::Failed;
                job.snapshot.error = Some(RuntimeJobFailure::infrastructure(error));
                self.finish_job(job)?;
            }
        }
        Ok(true)
    }

    fn ensure_layout(&self) -> Result<(), RuntimeError> {
        for dir in [
            self.queue_dir(),
            self.running_dir(),
            self.terminal_dir(),
            self.cache_dir(),
            self.root.join("artifacts/sha256"),
            self.root.join("cancel"),
        ] {
            fs::create_dir_all(dir)?;
        }
        Ok(())
    }

    fn claim_next(&self) -> Result<Option<StoredJob>, RuntimeError> {
        for source in json_files(&self.queue_dir())? {
            let name = source.file_name().ok_or_else(|| {
                RuntimeError::new("runtime_queue_invalid", "queue entry has no file name")
            })?;
            let destination = self.running_dir().join(name);
            match fs::rename(&source, &destination) {
                Ok(()) => {
                    let mut job = read_job(&destination)?;
                    job.snapshot.status = RuntimeJobStatus::Running;
                    job.snapshot.attempts = job.snapshot.attempts.saturating_add(1);
                    job.snapshot.error = None;
                    write_json_atomic(&destination, &job)?;
                    return Ok(Some(job));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(None)
    }

    fn finish_job(&self, job: StoredJob) -> Result<(), RuntimeError> {
        let running = self.running_path(&job.snapshot.id);
        write_json_atomic(&running, &job)?;
        fs::rename(running, self.terminal_path(&job.snapshot.id))?;
        Ok(())
    }

    fn requeue_job(&self, job: StoredJob) -> Result<(), RuntimeError> {
        let running = self.running_path(&job.snapshot.id);
        write_json_atomic(&running, &job)?;
        fs::rename(running, self.queue_path(&job.snapshot.id))?;
        Ok(())
    }

    fn read_cache(&self, key: &str) -> Result<Option<SimulationResult>, RuntimeError> {
        let path = self.cache_path(key);
        if !path.is_file() {
            return Ok(None);
        }
        Ok(Some(read_json(&path)?))
    }

    fn write_cache(&self, key: &str, result: &SimulationResult) -> Result<(), RuntimeError> {
        write_json_atomic(&self.cache_path(key), result)
    }

    fn prune_terminal(&self) -> Result<(), RuntimeError> {
        let mut files = json_files(&self.terminal_dir())?;
        if files.len() <= self.limits.max_retained_terminal_jobs {
            return Ok(());
        }
        files.sort();
        let remove_count = files.len() - self.limits.max_retained_terminal_jobs;
        for path in files.into_iter().take(remove_count) {
            fs::remove_file(path)?;
        }
        Ok(())
    }

    fn queue_dir(&self) -> PathBuf {
        self.root.join("queue")
    }

    fn running_dir(&self) -> PathBuf {
        self.root.join("running")
    }

    fn terminal_dir(&self) -> PathBuf {
        self.root.join("terminal")
    }

    fn cache_dir(&self) -> PathBuf {
        self.root.join("cache/sha256")
    }

    fn queue_path(&self, id: &RuntimeJobId) -> PathBuf {
        self.queue_dir().join(format!("{}.json", id.as_str()))
    }

    fn running_path(&self, id: &RuntimeJobId) -> PathBuf {
        self.running_dir().join(format!("{}.json", id.as_str()))
    }

    fn terminal_path(&self, id: &RuntimeJobId) -> PathBuf {
        self.terminal_dir().join(format!("{}.json", id.as_str()))
    }

    fn cancel_path(&self, id: &RuntimeJobId) -> PathBuf {
        self.root.join("cancel").join(id.as_str())
    }

    fn cache_path(&self, key: &str) -> PathBuf {
        self.cache_dir().join(format!("{key}.json"))
    }
}

#[derive(Clone)]
pub struct ContentAddressedStore {
    root: PathBuf,
}

impl ContentAddressedStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn put(&self, bytes: &[u8]) -> Result<ArtifactRef, RuntimeError> {
        let digest = sha256_hex(bytes);
        let path = self.path_for(&digest)?;
        if !path.exists() {
            write_bytes_atomic(&path, bytes)?;
        }
        Ok(ArtifactRef {
            sha256: digest,
            bytes: bytes.len() as u64,
        })
    }

    pub fn get(&self, artifact: &ArtifactRef) -> Result<Vec<u8>, RuntimeError> {
        let path = self.path_for(&artifact.sha256)?;
        let bytes = read_bounded(&path, artifact.bytes, "artifact")?;
        if bytes.len() as u64 != artifact.bytes || sha256_hex(&bytes) != artifact.sha256 {
            return Err(RuntimeError::new(
                "runtime_artifact_corrupt",
                "content-addressed artifact did not match its recorded digest/size",
            ));
        }
        Ok(bytes)
    }

    pub fn contains(&self, sha256: &str) -> bool {
        self.path_for(sha256).is_ok_and(|path| path.is_file())
    }

    fn path_for(&self, digest: &str) -> Result<PathBuf, RuntimeError> {
        if digest.len() != 64
            || !digest
                .as_bytes()
                .iter()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(RuntimeError::new(
                "runtime_artifact_invalid",
                "artifact SHA-256 must be 64 lowercase hexadecimal characters",
            ));
        }
        Ok(self.root.join(&digest[..2]).join(digest))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerRequestEnvelope {
    pub request: SimulationRequest,
    pub execution_policy: ExecutionPolicy,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerResponseEnvelope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<SimulationResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<SimulationError>,
}

impl WorkerResponseEnvelope {
    pub fn from_result(result: Result<SimulationResult, SimulationError>) -> Self {
        match result {
            Ok(result) => Self {
                result: Some(result),
                error: None,
            },
            Err(error) => Self {
                result: None,
                error: Some(error),
            },
        }
    }
}

pub fn default_worker_simulator() -> Simulator {
    let mut simulator = Simulator::new();
    simulator.register_engine(DigitalEngine::new());
    simulator.register_engine(NgSpiceEngine::default());
    simulator.register_engine(XSpiceEngine::default());
    simulator.register_engine(MixedSignalEngine::default());
    simulator.register_engine(VerilatorEngine::default());
    simulator
}

fn build_manifest(
    request: &SimulationRequest,
    policy: &ExecutionPolicy,
    executor: &dyn RuntimeExecutor,
) -> Result<ReproducibilityManifest, RuntimeError> {
    let request_bytes = serde_json::to_vec(request)?;
    let request_sha256 = sha256_hex(&request_bytes);
    let mut engine_descriptors = executor.engine_descriptors();
    engine_descriptors.sort_by(|left, right| left.id.cmp(&right.id));
    let executor_fingerprint = executor.fingerprint();
    if executor_fingerprint.trim().is_empty() {
        return Err(RuntimeError::new(
            "runtime_reproducibility_invalid",
            "runtime executor fingerprint must not be empty",
        ));
    }
    let cache_identity = serde_json::to_vec(&(
        RUNTIME_MANIFEST_SCHEMA_VERSION,
        VERSION,
        &request_sha256,
        &executor_fingerprint,
        &engine_descriptors,
        policy,
    ))?;
    Ok(ReproducibilityManifest {
        schema_version: RUNTIME_MANIFEST_SCHEMA_VERSION,
        core_version: VERSION.to_owned(),
        request_sha256,
        cache_key_sha256: sha256_hex(&cache_identity),
        executor_fingerprint,
        engine_descriptors,
        execution_policy: policy.clone(),
    })
}

fn worker_simulation_error(error: RuntimeError) -> SimulationError {
    SimulationError::Engine {
        engine: EngineId::new("isolated-worker"),
        error: EngineError::new(error.code, error.message).retryable(true),
    }
}

fn worker_engine_error(
    code: impl Into<String>,
    message: impl Into<String>,
    retryable: bool,
) -> SimulationError {
    SimulationError::Engine {
        engine: EngineId::new("isolated-worker"),
        error: EngineError::new(code, message).retryable(retryable),
    }
}

fn new_job_id(cache_key: &str) -> RuntimeJobId {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = UNIQUE_COUNTER.fetch_add(1, Ordering::Relaxed);
    RuntimeJobId(format!(
        "sim-{nanos:020x}-{}-{counter:016x}",
        cache_key.get(..12).unwrap_or(cache_key)
    ))
}

fn unique_temp_dir(prefix: &str) -> Result<PathBuf, RuntimeError> {
    let base = std::env::temp_dir();
    for _ in 0..16 {
        let counter = UNIQUE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = base.join(format!(
            "{prefix}-{}-{nanos:x}-{counter:x}",
            std::process::id()
        ));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(RuntimeError::new(
        "runtime_temp_failed",
        "could not allocate a unique worker directory",
    ))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

fn json_files(dir: &Path) -> Result<Vec<PathBuf>, RuntimeError> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension() == Some(OsStr::new("json")) && path.is_file() {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn count_json_files(dir: &Path) -> Result<usize, RuntimeError> {
    Ok(json_files(dir)?.len())
}

fn read_job(path: &Path) -> Result<StoredJob, RuntimeError> {
    read_json(path)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, RuntimeError> {
    let bytes = fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), RuntimeError> {
    write_bytes_atomic(path, &serde_json::to_vec(value)?)
}

fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<(), RuntimeError> {
    let parent = path.parent().ok_or_else(|| {
        RuntimeError::new("runtime_io_failed", "atomic write target has no parent")
    })?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(
        ".tmp-{}-{:016x}",
        std::process::id(),
        UNIQUE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = options.open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    match fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&temp);
            Err(error.into())
        }
    }
}

fn read_bounded(path: &Path, limit: u64, label: &str) -> Result<Vec<u8>, RuntimeError> {
    let metadata = fs::metadata(path).map_err(|error| {
        RuntimeError::new(
            "runtime_worker_protocol_invalid",
            format!("{label} is missing: {error}"),
        )
    })?;
    enforce_bytes(label, metadata.len(), limit)?;
    let mut file = File::open(path)?;
    let mut bytes = Vec::with_capacity(metadata.len().min(usize::MAX as u64) as usize);
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn enforce_bytes(label: &str, observed: u64, limit: u64) -> Result<(), RuntimeError> {
    if limit != 0 && observed > limit {
        return Err(RuntimeError::new(
            "runtime_resource_limit",
            format!("{label} exceeded {limit} bytes (observed {observed})"),
        ));
    }
    Ok(())
}

fn bounded_file(path: &Path, limit: u64) -> String {
    let fallback = "<worker log unavailable>".to_owned();
    let Ok(mut file) = File::open(path) else {
        return fallback;
    };
    let cap = if limit == 0 {
        64 * 1024
    } else {
        limit.min(64 * 1024)
    };
    let mut bytes = vec![0; cap as usize];
    let Ok(read) = file.read(&mut bytes) else {
        return fallback;
    };
    bytes.truncate(read);
    String::from_utf8_lossy(&bytes).trim().to_owned()
}

fn find_executable(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return fs::canonicalize(&candidate).ok().or(Some(candidate));
        }
    }
    None
}
