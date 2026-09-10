//! Explicit scheduler-native mixed-domain co-simulation bridges.
//!
//! R9.4 keeps domain conversion out of the master scheduler. These small
//! participants make digital-to-analog and analog-to-digital boundaries
//! visible in the co-simulation graph while preserving deterministic same-time
//! delta-cycle settlement.

use crate::{
    CoSimulationParticipant, CoSimulationPort, CoSimulationTime, CoSimulationValue, EngineError,
    ExecutionControl, LogicValue, PinDirection, Unit,
};

pub const DAC_COSIM_DIGITAL_IN: &str = "digital_in";
pub const DAC_COSIM_ANALOG_OUT: &str = "analog_out";
pub const ADC_COSIM_ANALOG_IN: &str = "analog_in";
pub const ADC_COSIM_DIGITAL_OUT: &str = "digital_out";

fn validate_id(id: &str, code: &str, kind: &str) -> Result<(), EngineError> {
    if id.trim().is_empty() {
        return Err(EngineError::new(
            code,
            format!("{kind} co-simulation participant id must not be empty"),
        ));
    }
    Ok(())
}

fn check_control(control: &ExecutionControl) -> Result<(), EngineError> {
    control.policy.validate()?;
    if control.cancellation.is_cancelled() {
        return Err(EngineError::new(
            "execution_cancelled",
            "co-simulation bridge execution was cancelled",
        ));
    }
    Ok(())
}

fn require_current_time(
    current: CoSimulationTime,
    requested: CoSimulationTime,
    code: &str,
    kind: &str,
) -> Result<(), EngineError> {
    if requested != current {
        return Err(EngineError::new(
            code,
            format!("{kind} co-simulation input writes must occur at the participant current time"),
        ));
    }
    Ok(())
}

/// Deterministic scalar digital-to-voltage bridge for the R9 co-simulation graph.
///
/// Unknown/high-impedance digital values are rejected rather than silently
/// inventing an analog voltage. R3 remains the authority for four-state logic;
/// real Renode and Verilator co-simulation boundaries are currently two-state.
pub struct DacCoSimulationParticipant {
    id: String,
    ports: [CoSimulationPort; 2],
    current_time: CoSimulationTime,
    low_voltage: f64,
    high_voltage: f64,
    input: LogicValue,
    output: f64,
}

impl DacCoSimulationParticipant {
    pub fn new(
        id: impl Into<String>,
        low_voltage: f64,
        high_voltage: f64,
    ) -> Result<Self, EngineError> {
        let id = id.into();
        validate_id(&id, "dac_cosim_participant_invalid", "DAC")?;
        if !low_voltage.is_finite() || !high_voltage.is_finite() || low_voltage >= high_voltage {
            return Err(EngineError::new(
                "dac_cosim_voltage_invalid",
                "DAC co-simulation voltages must be finite with low_voltage < high_voltage",
            ));
        }
        Ok(Self {
            id,
            ports: [
                CoSimulationPort::digital(DAC_COSIM_DIGITAL_IN, PinDirection::Input),
                CoSimulationPort::analog(DAC_COSIM_ANALOG_OUT, PinDirection::Output, Unit::Volt),
            ],
            current_time: CoSimulationTime::ZERO,
            low_voltage,
            high_voltage,
            input: LogicValue::Zero,
            output: low_voltage,
        })
    }

    pub const fn low_voltage(&self) -> f64 {
        self.low_voltage
    }

    pub const fn high_voltage(&self) -> f64 {
        self.high_voltage
    }
}

impl CoSimulationParticipant for DacCoSimulationParticipant {
    fn id(&self) -> &str {
        &self.id
    }

    fn ports(&self) -> &[CoSimulationPort] {
        &self.ports
    }

    fn current_time(&self) -> CoSimulationTime {
        self.current_time
    }

    fn initialize(&mut self, control: &ExecutionControl) -> Result<(), EngineError> {
        check_control(control)?;
        self.current_time = CoSimulationTime::ZERO;
        self.input = LogicValue::Zero;
        self.output = self.low_voltage;
        Ok(())
    }

    fn advance_to(
        &mut self,
        target: CoSimulationTime,
        control: &ExecutionControl,
    ) -> Result<(), EngineError> {
        check_control(control)?;
        if target < self.current_time {
            return Err(EngineError::new(
                "dac_cosim_time_invalid",
                "DAC co-simulation participant cannot move backwards in time",
            ));
        }
        self.current_time = target;
        Ok(())
    }

    fn read_output(&self, port: &str) -> Result<CoSimulationValue, EngineError> {
        if port != DAC_COSIM_ANALOG_OUT {
            return Err(EngineError::new(
                "dac_cosim_port_missing",
                format!(
                    "DAC co-simulation participant `{}` has no readable port `{port}`",
                    self.id
                ),
            ));
        }
        Ok(CoSimulationValue::Analog(self.output))
    }

    fn write_input(
        &mut self,
        port: &str,
        time: CoSimulationTime,
        value: &CoSimulationValue,
        control: &ExecutionControl,
    ) -> Result<bool, EngineError> {
        check_control(control)?;
        if port != DAC_COSIM_DIGITAL_IN {
            return Err(EngineError::new(
                "dac_cosim_port_missing",
                format!(
                    "DAC co-simulation participant `{}` has no writable port `{port}`",
                    self.id
                ),
            ));
        }
        require_current_time(self.current_time, time, "dac_cosim_time_invalid", "DAC")?;
        let CoSimulationValue::Digital(value) = value else {
            return Err(EngineError::new(
                "dac_cosim_domain_mismatch",
                "DAC co-simulation input requires a digital value",
            ));
        };
        if !value.is_known() {
            return Err(EngineError::new(
                "dac_cosim_two_state_input",
                "DAC co-simulation input requires deterministic zero/one; resolve X/Z before the bridge",
            ));
        }
        if *value == self.input {
            return Ok(false);
        }
        self.input = *value;
        self.output = if *value == LogicValue::One {
            self.high_voltage
        } else {
            self.low_voltage
        };
        Ok(true)
    }
}

/// Deterministic voltage-to-digital Schmitt bridge for the R9 co-simulation graph.
///
/// Values at/below `low_threshold` produce zero; values at/above
/// `high_threshold` produce one; values between thresholds retain the previous
/// known output. The initial output is low, making the hysteresis region fully
/// deterministic at time zero.
pub struct AdcCoSimulationParticipant {
    id: String,
    ports: [CoSimulationPort; 2],
    current_time: CoSimulationTime,
    low_threshold: f64,
    high_threshold: f64,
    input: f64,
    output: LogicValue,
}

impl AdcCoSimulationParticipant {
    pub fn new(
        id: impl Into<String>,
        low_threshold: f64,
        high_threshold: f64,
    ) -> Result<Self, EngineError> {
        let id = id.into();
        validate_id(&id, "adc_cosim_participant_invalid", "ADC")?;
        if !low_threshold.is_finite()
            || !high_threshold.is_finite()
            || low_threshold >= high_threshold
        {
            return Err(EngineError::new(
                "adc_cosim_threshold_invalid",
                "ADC co-simulation thresholds must be finite with low_threshold < high_threshold",
            ));
        }
        let output = classify_adc(0.0, LogicValue::Zero, low_threshold, high_threshold);
        Ok(Self {
            id,
            ports: [
                CoSimulationPort::analog(ADC_COSIM_ANALOG_IN, PinDirection::Input, Unit::Volt),
                CoSimulationPort::digital(ADC_COSIM_DIGITAL_OUT, PinDirection::Output),
            ],
            current_time: CoSimulationTime::ZERO,
            low_threshold,
            high_threshold,
            input: 0.0,
            output,
        })
    }

    pub const fn low_threshold(&self) -> f64 {
        self.low_threshold
    }

    pub const fn high_threshold(&self) -> f64 {
        self.high_threshold
    }
}

impl CoSimulationParticipant for AdcCoSimulationParticipant {
    fn id(&self) -> &str {
        &self.id
    }

    fn ports(&self) -> &[CoSimulationPort] {
        &self.ports
    }

    fn current_time(&self) -> CoSimulationTime {
        self.current_time
    }

    fn initialize(&mut self, control: &ExecutionControl) -> Result<(), EngineError> {
        check_control(control)?;
        self.current_time = CoSimulationTime::ZERO;
        self.input = 0.0;
        self.output = classify_adc(
            self.input,
            LogicValue::Zero,
            self.low_threshold,
            self.high_threshold,
        );
        Ok(())
    }

    fn advance_to(
        &mut self,
        target: CoSimulationTime,
        control: &ExecutionControl,
    ) -> Result<(), EngineError> {
        check_control(control)?;
        if target < self.current_time {
            return Err(EngineError::new(
                "adc_cosim_time_invalid",
                "ADC co-simulation participant cannot move backwards in time",
            ));
        }
        self.current_time = target;
        Ok(())
    }

    fn read_output(&self, port: &str) -> Result<CoSimulationValue, EngineError> {
        if port != ADC_COSIM_DIGITAL_OUT {
            return Err(EngineError::new(
                "adc_cosim_port_missing",
                format!(
                    "ADC co-simulation participant `{}` has no readable port `{port}`",
                    self.id
                ),
            ));
        }
        Ok(CoSimulationValue::Digital(self.output))
    }

    fn write_input(
        &mut self,
        port: &str,
        time: CoSimulationTime,
        value: &CoSimulationValue,
        control: &ExecutionControl,
    ) -> Result<bool, EngineError> {
        check_control(control)?;
        if port != ADC_COSIM_ANALOG_IN {
            return Err(EngineError::new(
                "adc_cosim_port_missing",
                format!(
                    "ADC co-simulation participant `{}` has no writable port `{port}`",
                    self.id
                ),
            ));
        }
        require_current_time(self.current_time, time, "adc_cosim_time_invalid", "ADC")?;
        let CoSimulationValue::Analog(value) = value else {
            return Err(EngineError::new(
                "adc_cosim_domain_mismatch",
                "ADC co-simulation input requires an analog value",
            ));
        };
        if !value.is_finite() {
            return Err(EngineError::new(
                "adc_cosim_value_invalid",
                "ADC co-simulation input must be finite",
            ));
        }
        if self.input.to_bits() == value.to_bits() {
            return Ok(false);
        }
        self.input = *value;
        self.output = classify_adc(
            self.input,
            self.output,
            self.low_threshold,
            self.high_threshold,
        );
        Ok(true)
    }
}

fn classify_adc(
    value: f64,
    previous: LogicValue,
    low_threshold: f64,
    high_threshold: f64,
) -> LogicValue {
    if value <= low_threshold {
        LogicValue::Zero
    } else if value >= high_threshold {
        LogicValue::One
    } else {
        previous
    }
}
