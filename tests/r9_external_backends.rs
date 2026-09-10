use ontologyx_sim_core::{
    Circuit, CoSimulationParticipant, CoSimulationTime, CoSimulationValue, Component,
    ComponentKind, ExecutionControl, ExecutionPolicy, LogicValue, Net, NetEndpoint,
    NgSpiceCoSimulationBinding, NgSpiceCoSimulationConfig, NgSpiceCoSimulationParticipant,
    ParameterValue, Pin, PinDirection, Quantity, RenodeCoSimulationBinding,
    RenodeCoSimulationConfig, RenodeCoSimulationParticipant, SignalDomain, Unit,
};

fn analog_pin(id: &str) -> Pin {
    Pin::new(id, id, SignalDomain::Analog, PinDirection::Passive)
}

fn ground() -> Component {
    Component::new("gnd", ComponentKind::ground()).with_pin(Pin::new(
        "gnd",
        "gnd",
        SignalDomain::Reference,
        PinDirection::Passive,
    ))
}

fn resistor(id: &str) -> Component {
    Component::new(id, ComponentKind::resistor())
        .with_pin(analog_pin("a"))
        .with_pin(analog_pin("b"))
        .with_parameter(
            "resistance",
            ParameterValue::Quantity(Quantity::new(1000.0, Unit::Ohm)),
        )
}

fn external_divider() -> Circuit {
    Circuit::new()
        .with_component(resistor("r1"))
        .with_component(resistor("r2"))
        .with_component(ground())
        .with_net(Net::new("vin").connect(NetEndpoint::new("r1", "a")))
        .with_net(
            Net::new("out")
                .connect(NetEndpoint::new("r1", "b"))
                .connect(NetEndpoint::new("r2", "a")),
        )
        .with_net(
            Net::new("gnd")
                .connect(NetEndpoint::new("r2", "b"))
                .connect(NetEndpoint::new("gnd", "gnd")),
        )
}

#[test]
fn r9_3_public_contracts_are_fail_closed_before_native_startup() {
    let error = RenodeCoSimulationConfig::new("0", "machine")
        .validate()
        .expect_err("port zero must fail");
    assert_eq!(error.code(), "renode_cosim_config_invalid");

    let error = RenodeCoSimulationParticipant::new(
        "mcu",
        RenodeCoSimulationConfig::new("12345", "machine"),
        vec![RenodeCoSimulationBinding::input("gpio", "sysbus.gpio", -1)],
    )
    .err()
    .expect("negative GPIO pin must fail");
    assert_eq!(error.code(), "renode_cosim_binding_invalid");

    let error = NgSpiceCoSimulationParticipant::new(
        "analog",
        external_divider(),
        NgSpiceCoSimulationConfig::new(
            CoSimulationTime::from_picoseconds(10_000),
            CoSimulationTime::from_picoseconds(1_000),
        ),
        vec![NgSpiceCoSimulationBinding::output(
            "missing",
            "does-not-exist",
        )],
    )
    .err()
    .expect("unknown analog net must fail");
    assert_eq!(error.code(), "ngspice_cosim_binding_invalid");
}

#[test]
fn r9_3_native_helpers_are_bound_to_official_incremental_apis() {
    let renode = include_str!("../native/renode-cosim-helper.c");
    assert!(renode.contains("renode_connect"));
    assert!(renode.contains("renode_run_for"));
    assert!(renode.contains("renode_get_current_time"));
    assert!(renode.contains("renode_get_gpio_state"));
    assert!(renode.contains("renode_set_gpio_state"));
    assert!(renode.contains("renode_set_fatal_error_callback"));

    let ngspice = include_str!("../native/ngspice-cosim-helper.c");
    assert!(ngspice.contains("ngSpice_Init_Sync"));
    assert!(!ngspice.contains("ngSpice_nospiceinit() != 0"));
    assert!(ngspice.contains("reject_ambient_user_spiceinit"));
    assert!(ngspice.contains("before-ngSpice_Init"));
    assert!(ngspice.contains("after-ngSpice_Circ"));
    assert!(ngspice.contains("ngSpice_Circ"));
    assert!(ngspice.contains("stop when time ="));
    assert!(ngspice.contains("bg_run"));
    assert!(ngspice.contains("bg_resume"));
    assert!(ngspice.contains("ngGet_Vec_Info"));
}

#[cfg(unix)]
mod unix_protocol {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
        thread,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use super::*;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempExecutable {
        dir: PathBuf,
        path: PathBuf,
    }

    impl TempExecutable {
        fn new(name: &str, body: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "ontologyx-sim-core-r9-3-{}-{nonce}-{counter}",
                std::process::id()
            ));
            fs::create_dir(&dir).unwrap();
            let path = dir.join(name);
            fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
            fs::File::open(&path).unwrap().sync_all().unwrap();
            let mut permissions = fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&path, permissions).unwrap();
            Self { dir, path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempExecutable {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn renode_participant_owns_exact_virtual_time_and_same_time_gpio_exchange() {
        let helper = TempExecutable::new(
            "fake-renode-helper",
            r#"
time=0
input=0
output=0
printf 'STATE %s %s %s\n' "$time" "$input" "$output"
while IFS= read -r command; do
  case "$command" in
    'SET 0 0') input=0; output=0 ;;
    'SET 0 1') input=1; output=1 ;;
    'ADV '*) time=${command#ADV } ;;
    QUIT) exit 0 ;;
    *) exit 9 ;;
  esac
  printf 'STATE %s %s %s\n' "$time" "$input" "$output"
done
"#,
        );
        let config = RenodeCoSimulationConfig::new("12345", "machine").with_helper(helper.path());
        let mut participant = RenodeCoSimulationParticipant::new(
            "mcu",
            config,
            vec![
                RenodeCoSimulationBinding::input("in", "sysbus.gpio", 1),
                RenodeCoSimulationBinding::output("out", "sysbus.gpio", 2),
            ],
        )
        .unwrap();
        let control = ExecutionControl::default();
        participant.initialize(&control).unwrap();
        assert_eq!(participant.current_time(), CoSimulationTime::ZERO);
        assert_eq!(
            participant.next_event_time(),
            Some(CoSimulationTime::from_picoseconds(1_000))
        );
        assert!(
            participant
                .write_input(
                    "in",
                    CoSimulationTime::ZERO,
                    &CoSimulationValue::Digital(LogicValue::One),
                    &control,
                )
                .unwrap()
        );
        assert_eq!(
            participant.read_output("out").unwrap(),
            CoSimulationValue::Digital(LogicValue::One)
        );
        participant
            .advance_to(CoSimulationTime::from_picoseconds(4_000), &control)
            .unwrap();
        assert_eq!(
            participant.current_time(),
            CoSimulationTime::from_picoseconds(4_000)
        );
        let error = participant
            .advance_to(CoSimulationTime::from_picoseconds(4_500), &control)
            .expect_err("sub-ns Renode target must fail closed");
        assert_eq!(error.code(), "renode_cosim_time_resolution");
    }

    #[test]
    fn sharedspice_participant_keeps_one_session_and_exchanges_analog_values() {
        let helper = TempExecutable::new(
            "fake-ngspice-helper",
            r#"
time=0
output=0.0
printf 'STATE %s %s\n' "$time" "$output"
while IFS= read -r command; do
  case "$command" in
    'SET 0 '*) output=2.0 ;;
    'ADV '*) time=${command#ADV } ;;
    QUIT) exit 0 ;;
    *) exit 9 ;;
  esac
  printf 'STATE %s %s\n' "$time" "$output"
done
"#,
        );
        let config = NgSpiceCoSimulationConfig::new(
            CoSimulationTime::from_picoseconds(20_000),
            CoSimulationTime::from_picoseconds(1_000),
        )
        .with_helper(helper.path());
        let mut participant = NgSpiceCoSimulationParticipant::new(
            "analog",
            external_divider(),
            config,
            vec![
                NgSpiceCoSimulationBinding::input("vin", "vin"),
                NgSpiceCoSimulationBinding::output("vout", "out"),
            ],
        )
        .unwrap();
        let control = ExecutionControl::default();
        participant.initialize(&control).unwrap();
        assert_eq!(
            participant.read_output("vout").unwrap(),
            CoSimulationValue::Analog(0.0)
        );
        assert!(
            participant
                .write_input(
                    "vin",
                    CoSimulationTime::ZERO,
                    &CoSimulationValue::Analog(4.0),
                    &control,
                )
                .unwrap()
        );
        assert_eq!(
            participant.read_output("vout").unwrap(),
            CoSimulationValue::Analog(2.0)
        );
        participant
            .advance_to(CoSimulationTime::from_picoseconds(5_000), &control)
            .unwrap();
        assert_eq!(
            participant.current_time(),
            CoSimulationTime::from_picoseconds(5_000)
        );
        assert!(
            !participant
                .write_input(
                    "vin",
                    CoSimulationTime::from_picoseconds(5_000),
                    &CoSimulationValue::Analog(4.0),
                    &control,
                )
                .unwrap()
        );
    }

    #[test]
    fn stalled_native_helper_is_bounded_by_execution_policy() {
        let helper = TempExecutable::new("stalled-helper", "exec sleep 5");
        let mut participant = RenodeCoSimulationParticipant::new(
            "mcu",
            RenodeCoSimulationConfig::new("12345", "machine").with_helper(helper.path()),
            vec![RenodeCoSimulationBinding::output("out", "sysbus.gpio", 1)],
        )
        .unwrap();
        let policy = ExecutionPolicy {
            timeout_ms: 40,
            poll_interval_ms: 5,
            ..ExecutionPolicy::default()
        };
        let error = participant
            .initialize(&ExecutionControl::new(policy))
            .expect_err("stalled helper must time out instead of blocking the scheduler");
        assert_eq!(
            error.code(),
            "execution_timeout",
            "unexpected error: {error}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn transient_busy_helper_spawn_is_retried_within_execution_policy() {
        let helper = TempExecutable::new(
            "temporarily-busy-helper",
            r#"
printf 'STATE 0 0\n'
while IFS= read -r command; do
  case "$command" in
    QUIT) exit 0 ;;
    *) printf 'STATE 0 0\n' ;;
  esac
done
"#,
        );
        let busy = fs::OpenOptions::new()
            .write(true)
            .open(helper.path())
            .unwrap();
        let releaser = thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            drop(busy);
        });

        let mut participant = RenodeCoSimulationParticipant::new(
            "mcu",
            RenodeCoSimulationConfig::new("12345", "machine").with_helper(helper.path()),
            vec![RenodeCoSimulationBinding::output("out", "sysbus.gpio", 1)],
        )
        .unwrap();
        let policy = ExecutionPolicy {
            timeout_ms: 500,
            poll_interval_ms: 5,
            ..ExecutionPolicy::default()
        };
        participant
            .initialize(&ExecutionControl::new(policy))
            .expect("transient ETXTBSY helper startup must recover within policy");
        releaser.join().unwrap();
    }

    #[test]
    fn helper_protocol_failure_is_normalized_instead_of_panicking_host() {
        let helper = TempExecutable::new("broken-helper", "printf 'BROKEN\\n'");
        let mut participant = RenodeCoSimulationParticipant::new(
            "mcu",
            RenodeCoSimulationConfig::new("12345", "machine").with_helper(helper.path()),
            vec![RenodeCoSimulationBinding::output("out", "sysbus.gpio", 1)],
        )
        .unwrap();
        let error = participant
            .initialize(&ExecutionControl::default())
            .expect_err("malformed helper state must fail closed");
        assert_eq!(error.code(), "renode_cosim_protocol_error");
    }
}
