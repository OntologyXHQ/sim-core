//! Cargo-facing snapshot command.
//!
//! `cargo snapshot` is configured in `.cargo/config.toml` to run this example,
//! which delegates the archive policy to `scripts/snapshot.sh`.

use std::{
    env,
    path::PathBuf,
    process::{Command, ExitCode},
};

fn main() -> ExitCode {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script = root.join("scripts/snapshot.sh");

    let status = Command::new("bash")
        .arg(&script)
        .args(env::args_os().skip(1))
        .current_dir(&root)
        .status();

    match status {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => {
            eprintln!("[sim-core] snapshot command failed with {status}");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("[sim-core] could not execute {}: {error}", script.display());
            ExitCode::FAILURE
        }
    }
}
