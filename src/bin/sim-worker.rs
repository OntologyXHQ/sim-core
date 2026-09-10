use std::{error::Error, fs, path::PathBuf};

use ontologyx_sim_core::{
    ExecutionControl, WorkerRequestEnvelope, WorkerResponseEnvelope, default_worker_simulator,
};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let mut request_path = None::<PathBuf>;
    let mut response_path = None::<PathBuf>;

    while let Some(arg) = args.next() {
        match arg.to_string_lossy().as_ref() {
            "--request" => request_path = args.next().map(PathBuf::from),
            "--response" => response_path = args.next().map(PathBuf::from),
            other => return Err(format!("unknown argument: {other}").into()),
        }
    }

    let request_path = request_path.ok_or("missing --request")?;
    let response_path = response_path.ok_or("missing --response")?;
    let envelope: WorkerRequestEnvelope = serde_json::from_slice(&fs::read(&request_path)?)?;

    let simulator = default_worker_simulator();
    let result = simulator.simulate_with_control(
        &envelope.request,
        &ExecutionControl::new(envelope.execution_policy),
    );
    let response = WorkerResponseEnvelope::from_result(result);
    fs::write(response_path, serde_json::to_vec(&response)?)?;
    Ok(())
}
