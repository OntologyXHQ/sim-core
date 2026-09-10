#![cfg(feature = "production")]

use std::{
    fs,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use ontologyx_sim_core::{
    Analysis, AnalysisKind, Circuit, Component, ComponentKind, DigitalEngine, EngineCapabilities,
    EngineDescriptor, EngineError, EngineId, ExecutionControl, ExecutionPolicy, Net, NetEndpoint,
    ParameterValue, Pin, PinDirection, Probe, ProductionRuntime, ProductionRuntimeLimits,
    RuntimeExecutor, RuntimeJobStatus, SignalDomain, SimulationEngine, SimulationError,
    SimulationRequest, SimulationResult,
};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(1);

fn pin(id: &str, direction: PinDirection) -> Pin {
    Pin::new(id, id, SignalDomain::Digital, direction)
}

fn request(value: i64) -> SimulationRequest {
    let source = Component::new("source", ComponentKind::logic_input())
        .with_pin(pin("out", PinDirection::Output))
        .with_parameter("value", ParameterValue::Integer(value));
    let sink = Component::new("sink", ComponentKind::logic_output())
        .with_pin(pin("in", PinDirection::Input));
    SimulationRequest {
        circuit: Circuit::new()
            .with_component(source)
            .with_component(sink)
            .with_net(
                Net::new("signal")
                    .connect(NetEndpoint::new("source", "out"))
                    .connect(NetEndpoint::new("sink", "in")),
            ),
        analysis: Analysis::DigitalTransient { stop: 1e-6 },
        probes: vec![Probe {
            endpoint: NetEndpoint::new("sink", "in"),
            alias: Some(format!("signal-{value}")),
        }],
    }
}

fn temp_root(label: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "ontologyx-r8-{label}-{}-{nanos:x}-{:x}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&path).unwrap();
    path
}

#[derive(Clone)]
struct CountingExecutor {
    calls: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
    delay: Duration,
    fail_first: Arc<AtomicUsize>,
}

impl CountingExecutor {
    fn new(delay: Duration) -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            active: Arc::new(AtomicUsize::new(0)),
            max_active: Arc::new(AtomicUsize::new(0)),
            delay,
            fail_first: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn with_retryable_first_failure(self) -> Self {
        self.fail_first.store(1, Ordering::Relaxed);
        self
    }

    fn update_max(&self, observed: usize) {
        let mut current = self.max_active.load(Ordering::Relaxed);
        while observed > current {
            match self.max_active.compare_exchange_weak(
                current,
                observed,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(next) => current = next,
            }
        }
    }
}

impl RuntimeExecutor for CountingExecutor {
    fn engine_descriptors(&self) -> Vec<EngineDescriptor> {
        vec![EngineDescriptor {
            id: EngineId::new("digital"),
            version: Some(ontologyx_sim_core::VERSION.to_owned()),
            capabilities: EngineCapabilities {
                analog: false,
                digital: true,
                mixed_signal: false,
                analyses: [AnalysisKind::DigitalTransient].into_iter().collect(),
            },
        }]
    }

    fn fingerprint(&self) -> String {
        "counting-executor-v1".to_owned()
    }

    fn execute(
        &self,
        request: &SimulationRequest,
        control: &ExecutionControl,
    ) -> Result<SimulationResult, SimulationError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.update_max(active);
        if !self.delay.is_zero() {
            thread::sleep(self.delay);
        }
        self.active.fetch_sub(1, Ordering::SeqCst);

        if self
            .fail_first
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                value.checked_sub(1)
            })
            .is_ok()
        {
            return Err(SimulationError::Engine {
                engine: EngineId::new("digital"),
                error: EngineError::new("temporary_worker_failure", "retry me").retryable(true),
            });
        }

        DigitalEngine::new()
            .simulate_with_control(request, control)
            .map_err(|error| SimulationError::Engine {
                engine: EngineId::new("digital"),
                error,
            })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn durable_runtime_caches_by_reproducibility_manifest() {
    let root = temp_root("cache");
    let executor = CountingExecutor::new(Duration::ZERO);
    let calls = executor.calls.clone();
    let runtime = ProductionRuntime::open(
        &root,
        Arc::new(executor),
        ProductionRuntimeLimits::default(),
    )
    .unwrap();

    let first = runtime.submit(request(1)).unwrap();
    assert_eq!(first.status, RuntimeJobStatus::Queued);
    assert_eq!(runtime.drain().await.unwrap(), 1);

    let first = runtime.get(&first.id).unwrap();
    assert_eq!(first.status, RuntimeJobStatus::Succeeded);
    assert_eq!(first.attempts, 1);
    assert!(!first.cache_hit);
    assert_eq!(calls.load(Ordering::Relaxed), 1);

    let second = runtime.submit(request(1)).unwrap();
    assert_eq!(second.status, RuntimeJobStatus::Succeeded);
    assert!(second.cache_hit);
    assert_eq!(
        first.manifest.cache_key_sha256,
        second.manifest.cache_key_sha256
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worker_pool_executes_independent_jobs_in_parallel() {
    let root = temp_root("parallel");
    let executor = CountingExecutor::new(Duration::from_millis(50));
    let max_active = executor.max_active.clone();
    let limits = ProductionRuntimeLimits {
        max_parallel_workers: 4,
        ..ProductionRuntimeLimits::default()
    };
    let runtime = ProductionRuntime::open(&root, Arc::new(executor), limits).unwrap();

    for value in 0..8 {
        runtime.submit(request(value)).unwrap();
    }
    assert_eq!(runtime.drain().await.unwrap(), 8);
    assert!(max_active.load(Ordering::Relaxed) >= 2);

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retryable_worker_failure_is_requeued_and_then_succeeds() {
    let root = temp_root("retry");
    let executor = CountingExecutor::new(Duration::ZERO).with_retryable_first_failure();
    let calls = executor.calls.clone();
    let limits = ProductionRuntimeLimits {
        max_parallel_workers: 1,
        max_attempts: 2,
        ..ProductionRuntimeLimits::default()
    };
    let runtime = ProductionRuntime::open(&root, Arc::new(executor), limits).unwrap();
    let submitted = runtime.submit(request(1)).unwrap();

    assert_eq!(runtime.drain().await.unwrap(), 2);
    let terminal = runtime.get(&submitted.id).unwrap();
    assert_eq!(terminal.status, RuntimeJobStatus::Succeeded);
    assert_eq!(terminal.attempts, 2);
    assert_eq!(calls.load(Ordering::Relaxed), 2);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn running_jobs_are_recovered_to_the_durable_queue() {
    let root = temp_root("recovery");
    let executor = CountingExecutor::new(Duration::ZERO);
    let runtime = ProductionRuntime::open(
        &root,
        Arc::new(executor.clone()),
        ProductionRuntimeLimits::default(),
    )
    .unwrap();
    let submitted = runtime.submit(request(1)).unwrap();
    let queued = root
        .join("queue")
        .join(format!("{}.json", submitted.id.as_str()));
    let running = root
        .join("running")
        .join(format!("{}.json", submitted.id.as_str()));
    fs::rename(queued, running).unwrap();
    drop(runtime);

    let reopened = ProductionRuntime::open(
        &root,
        Arc::new(executor),
        ProductionRuntimeLimits::default(),
    )
    .unwrap();
    assert_eq!(
        reopened.get(&submitted.id).unwrap().status,
        RuntimeJobStatus::Queued
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn content_addressed_artifacts_deduplicate_and_verify_digest() {
    let root = temp_root("artifacts");
    let runtime = ProductionRuntime::open(
        &root,
        Arc::new(CountingExecutor::new(Duration::ZERO)),
        ProductionRuntimeLimits::default(),
    )
    .unwrap();
    let store = runtime.content_store();
    let first = store.put(b"firmware-or-model-bytes").unwrap();
    let second = store.put(b"firmware-or-model-bytes").unwrap();
    assert_eq!(first, second);
    assert!(store.contains(&first.sha256));
    assert_eq!(store.get(&first).unwrap(), b"firmware-or-model-bytes");
    assert!(
        root.join("artifacts/sha256")
            .join(&first.sha256[..2])
            .join(&first.sha256)
            .is_file()
    );
    assert!(!store.contains("../../escape"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn production_limits_are_fail_closed() {
    let invalid = ProductionRuntimeLimits {
        max_parallel_workers: 0,
        ..ProductionRuntimeLimits::default()
    };
    assert_eq!(
        invalid.validate().unwrap_err().code,
        "runtime_config_invalid"
    );

    let invalid_policy = ProductionRuntimeLimits {
        execution_policy: ExecutionPolicy {
            poll_interval_ms: 0,
            ..ExecutionPolicy::default()
        },
        ..ProductionRuntimeLimits::default()
    };
    assert_eq!(
        invalid_policy.validate().unwrap_err().code,
        "runtime_config_invalid"
    );
}
