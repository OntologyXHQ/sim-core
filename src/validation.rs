use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    CIRCUIT_SCHEMA_VERSION, Circuit, Component, ComponentKind, ModelKind, ModelLanguage,
    ParameterValue, PinDirection, SignalDomain, Unit,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueSeverity {
    Warning,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ValidationIssue {
    pub severity: IssueSeverity,
    pub code: String,
    pub path: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ValidationReport {
    #[serde(default)]
    pub issues: Vec<ValidationIssue>,
}

impl ValidationReport {
    pub fn is_valid(&self) -> bool {
        !self
            .issues
            .iter()
            .any(|issue| issue.severity == IssueSeverity::Error)
    }

    pub fn error_count(&self) -> usize {
        self.issues
            .iter()
            .filter(|issue| issue.severity == IssueSeverity::Error)
            .count()
    }

    pub fn warning_count(&self) -> usize {
        self.issues
            .iter()
            .filter(|issue| issue.severity == IssueSeverity::Warning)
            .count()
    }

    fn push(
        &mut self,
        severity: IssueSeverity,
        code: &str,
        path: String,
        message: impl Into<String>,
    ) {
        self.issues.push(ValidationIssue {
            severity,
            code: code.to_owned(),
            path,
            message: message.into(),
        });
    }

    fn error(&mut self, code: &str, path: String, message: impl Into<String>) {
        self.push(IssueSeverity::Error, code, path, message);
    }

    fn warning(&mut self, code: &str, path: String, message: impl Into<String>) {
        self.push(IssueSeverity::Warning, code, path, message);
    }
}

pub fn validate_circuit(circuit: &Circuit) -> ValidationReport {
    let mut report = ValidationReport::default();

    if circuit.schema_version != CIRCUIT_SCHEMA_VERSION {
        report.error(
            "unsupported_schema_version",
            "schema_version".into(),
            format!(
                "expected schema version {CIRCUIT_SCHEMA_VERSION}, got {}",
                circuit.schema_version
            ),
        );
    }

    let mut models = BTreeMap::new();
    for (model_index, model) in circuit.models.iter().enumerate() {
        let model_path = format!("models[{model_index}]");
        let model_id = model.id.as_str().trim();
        if model_id.is_empty() {
            report.error(
                "empty_model_id",
                format!("{model_path}.id"),
                "model id must not be empty",
            );
        } else if models
            .insert(model_id, (model.kind, model.language))
            .is_some()
        {
            report.error(
                "duplicate_model_id",
                format!("{model_path}.id"),
                format!("duplicate model id `{model_id}`"),
            );
        }
        if model.entry.trim().is_empty() {
            report.error(
                "empty_model_entry",
                format!("{model_path}.entry"),
                "model entry must not be empty",
            );
        }
        if model.source.trim().is_empty() {
            report.error(
                "empty_model_source",
                format!("{model_path}.source"),
                "model source must not be empty",
            );
        }
        let compatible_language = match model.kind {
            ModelKind::Device | ModelKind::Subcircuit => model.language == ModelLanguage::Spice,
            ModelKind::Module => matches!(
                model.language,
                ModelLanguage::Verilog | ModelLanguage::SystemVerilog
            ),
        };
        if !compatible_language {
            report.error(
                "incompatible_model_language",
                format!("{model_path}.language"),
                format!(
                    "model kind {:?} is incompatible with language {:?}",
                    model.kind, model.language
                ),
            );
        }
    }

    let mut components: BTreeMap<&str, BTreeMap<&str, (SignalDomain, PinDirection)>> =
        BTreeMap::new();

    for (component_index, component) in circuit.components.iter().enumerate() {
        let component_path = format!("components[{component_index}]");
        let component_id = component.id.as_str().trim();
        if component_id.is_empty() {
            report.error(
                "empty_component_id",
                format!("{component_path}.id"),
                "component id must not be empty",
            );
            continue;
        }
        if component.kind.as_str().trim().is_empty() {
            report.error(
                "empty_component_kind",
                format!("{component_path}.kind"),
                "component kind must not be empty",
            );
        }
        if components.contains_key(component_id) {
            report.error(
                "duplicate_component_id",
                format!("{component_path}.id"),
                format!("duplicate component id `{component_id}`"),
            );
            continue;
        }

        let mut pins = BTreeMap::new();
        for (pin_index, pin) in component.pins.iter().enumerate() {
            let pin_path = format!("{component_path}.pins[{pin_index}]");
            let pin_id = pin.id.as_str().trim();
            if pin_id.is_empty() {
                report.error(
                    "empty_pin_id",
                    format!("{pin_path}.id"),
                    "pin id must not be empty",
                );
                continue;
            }
            if pins.insert(pin_id, (pin.domain, pin.direction)).is_some() {
                report.error(
                    "duplicate_pin_id",
                    format!("{pin_path}.id"),
                    format!("duplicate pin id `{pin_id}` on component `{component_id}`"),
                );
            }
        }
        if component.pins.is_empty() {
            report.warning(
                "component_has_no_pins",
                format!("{component_path}.pins"),
                "component has no pins",
            );
        }

        for (key, value) in &component.parameters {
            if key.trim().is_empty() {
                report.error(
                    "empty_parameter_name",
                    format!("{component_path}.parameters"),
                    "parameter name must not be empty",
                );
            }
            let finite = match value {
                ParameterValue::Number(value) => value.is_finite(),
                ParameterValue::Quantity(value) => value.is_finite(),
                _ => true,
            };
            if !finite {
                report.error(
                    "non_finite_parameter",
                    format!("{component_path}.parameters.{key}"),
                    "numeric parameters must be finite",
                );
            }
        }

        let expected_model_kind = if component.kind == ComponentKind::diode()
            || component.kind == ComponentKind::bjt()
            || component.kind == ComponentKind::mosfet()
        {
            Some(ModelKind::Device)
        } else if component.kind == ComponentKind::op_amp()
            || component.kind == ComponentKind::subcircuit()
        {
            Some(ModelKind::Subcircuit)
        } else {
            None
        };
        if let Some(expected_model_kind) = expected_model_kind {
            match component.parameters.get("model") {
                Some(ParameterValue::Text(model_id)) if !model_id.trim().is_empty() => {
                    match models.get(model_id.as_str()) {
                        None => report.error(
                            "unknown_model",
                            format!("{component_path}.parameters.model"),
                            format!("unknown model `{model_id}`"),
                        ),
                        Some((actual_kind, _)) if *actual_kind != expected_model_kind => report.error(
                            "incompatible_model_kind",
                            format!("{component_path}.parameters.model"),
                            format!(
                                "model `{model_id}` has kind {actual_kind:?}, expected {expected_model_kind:?}"
                            ),
                        ),
                        Some(_) => {}
                    }
                }
                Some(_) => report.error(
                    "invalid_model_reference",
                    format!("{component_path}.parameters.model"),
                    "model reference must be a non-empty string",
                ),
                None => report.error(
                    "missing_model_reference",
                    format!("{component_path}.parameters.model"),
                    "component requires a `model` reference",
                ),
            }
        }

        validate_mixed_signal_bridge(component, &component_path, &mut report);
        validate_hdl_module(component, &component_path, &models, &mut report);
        validate_mcu_component(component, &component_path, &mut report);

        components.insert(component_id, pins);
    }

    let mut net_ids = BTreeSet::new();
    for (net_index, net) in circuit.nets.iter().enumerate() {
        let net_path = format!("nets[{net_index}]");
        let net_id = net.id.as_str().trim();
        if net_id.is_empty() {
            report.error(
                "empty_net_id",
                format!("{net_path}.id"),
                "net id must not be empty",
            );
        } else if !net_ids.insert(net_id) {
            report.error(
                "duplicate_net_id",
                format!("{net_path}.id"),
                format!("duplicate net id `{net_id}`"),
            );
        }

        if net.endpoints.len() < 2 {
            report.warning(
                "dangling_net",
                format!("{net_path}.endpoints"),
                "net has fewer than two endpoints",
            );
        }

        let mut endpoints = BTreeSet::new();
        let mut domains = BTreeSet::new();
        let mut digital_drivers = 0_usize;
        for (endpoint_index, endpoint) in net.endpoints.iter().enumerate() {
            let endpoint_path = format!("{net_path}.endpoints[{endpoint_index}]");
            let key = (endpoint.component.as_str(), endpoint.pin.as_str());
            if !endpoints.insert(key) {
                report.error(
                    "duplicate_net_endpoint",
                    endpoint_path.clone(),
                    "the same endpoint appears more than once on this net",
                );
            }

            let Some(pins) = components.get(endpoint.component.as_str()) else {
                report.error(
                    "unknown_component",
                    format!("{endpoint_path}.component"),
                    format!("unknown component `{}`", endpoint.component.as_str()),
                );
                continue;
            };
            let Some((domain, direction)) = pins.get(endpoint.pin.as_str()) else {
                report.error(
                    "unknown_pin",
                    format!("{endpoint_path}.pin"),
                    format!(
                        "unknown pin `{}` on component `{}`",
                        endpoint.pin.as_str(),
                        endpoint.component.as_str()
                    ),
                );
                continue;
            };
            domains.insert(*domain);
            if *domain == SignalDomain::Digital
                && matches!(
                    *direction,
                    PinDirection::Output | PinDirection::Bidirectional
                )
            {
                digital_drivers += 1;
            }
        }

        if domains == BTreeSet::from([SignalDomain::Digital]) && digital_drivers > 1 {
            report.warning(
                "multiple_digital_drivers_resolved",
                format!("{net_path}.endpoints"),
                "multiple digital drivers are resolved by the event engine using Z/X contention semantics",
            );
        }

        if domains.contains(&SignalDomain::Digital)
            && (domains.contains(&SignalDomain::Analog)
                || domains.contains(&SignalDomain::Reference))
        {
            report.error(
                "mixed_signal_bridge_required",
                format!("{net_path}.endpoints"),
                "analog/reference and digital pins cannot share one net; connect them through separate adc_bridge/dac_bridge pins",
            );
        }
    }

    report
}

fn validate_mcu_component(
    component: &Component,
    component_path: &str,
    report: &mut ValidationReport,
) {
    if component.kind != ComponentKind::mcu() {
        return;
    }

    if component.pins.is_empty() {
        report.error(
            "mcu_has_no_pins",
            format!("{component_path}.pins"),
            "mcu must expose at least one GPIO output pin in R6",
        );
        return;
    }

    let mut mappings = BTreeSet::new();
    for (pin_index, pin) in component.pins.iter().enumerate() {
        let pin_path = format!("{component_path}.pins[{pin_index}]");
        if pin.domain != SignalDomain::Digital {
            report.error(
                "mcu_pin_domain",
                format!("{pin_path}.domain"),
                format!(
                    "MCU pin `{}` must use the digital domain in R6",
                    pin.id.as_str()
                ),
            );
        }
        if pin.direction != PinDirection::Output {
            report.error(
                "mcu_pin_direction",
                format!("{pin_path}.direction"),
                format!(
                    "MCU pin `{}` must be Output in R6; input/bidirectional synchronization belongs to the shared co-simulation scheduler",
                    pin.id.as_str()
                ),
            );
        }
        let mapping = pin.name.trim();
        if !is_mcu_gpio_mapping(mapping) {
            report.error(
                "mcu_pin_mapping_invalid",
                format!("{pin_path}.name"),
                format!(
                    "MCU pin `{}` name must map to a Renode GPIO as `peripheral@pin`, for example `gpioPortD@12`",
                    pin.id.as_str()
                ),
            );
        } else if !mappings.insert(mapping.to_owned()) {
            report.error(
                "mcu_pin_mapping_duplicate",
                format!("{pin_path}.name"),
                format!("Renode GPIO mapping `{mapping}` appears more than once"),
            );
        }
    }
}

fn is_mcu_gpio_mapping(value: &str) -> bool {
    let Some((peripheral, pin)) = value.split_once('@') else {
        return false;
    };
    is_renode_identifier(peripheral) && pin.parse::<u32>().is_ok()
}

fn is_renode_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn validate_hdl_module(
    component: &Component,
    component_path: &str,
    models: &BTreeMap<&str, (ModelKind, ModelLanguage)>,
    report: &mut ValidationReport,
) {
    if component.kind != ComponentKind::hdl_module() {
        return;
    }

    match component.parameters.get("model") {
        Some(ParameterValue::Text(model_id)) if !model_id.trim().is_empty() => {
            match models.get(model_id.as_str()) {
                None => report.error(
                    "unknown_model",
                    format!("{component_path}.parameters.model"),
                    format!("unknown HDL model `{model_id}`"),
                ),
                Some((ModelKind::Module, ModelLanguage::Verilog | ModelLanguage::SystemVerilog)) => {}
                Some((kind, language)) => report.error(
                    "incompatible_hdl_model",
                    format!("{component_path}.parameters.model"),
                    format!(
                        "HDL component requires a Verilog/SystemVerilog module model; `{model_id}` is {language:?}/{kind:?}"
                    ),
                ),
            }
        }
        Some(_) => report.error(
            "invalid_model_reference",
            format!("{component_path}.parameters.model"),
            "HDL model reference must be a non-empty string",
        ),
        None => report.error(
            "missing_model_reference",
            format!("{component_path}.parameters.model"),
            "hdl_module requires a `model` reference",
        ),
    }

    if component.pins.is_empty() {
        report.error(
            "hdl_module_has_no_pins",
            format!("{component_path}.pins"),
            "hdl_module must expose at least one digital input/output pin",
        );
    }
    let mut mappings = BTreeSet::new();
    for (pin_index, pin) in component.pins.iter().enumerate() {
        let pin_path = format!("{component_path}.pins[{pin_index}]");
        if pin.domain != SignalDomain::Digital {
            report.error(
                "hdl_pin_domain",
                format!("{pin_path}.domain"),
                format!("HDL pin `{}` must use the digital domain", pin.id.as_str()),
            );
        }
        if !matches!(pin.direction, PinDirection::Input | PinDirection::Output) {
            report.error(
                "hdl_pin_direction",
                format!("{pin_path}.direction"),
                format!(
                    "HDL pin `{}` must be Input or Output in R5; inout is deferred to a later co-simulation boundary",
                    pin.id.as_str()
                ),
            );
        }
        let mapping = pin.name.trim();
        if !is_hdl_port_reference(mapping) {
            report.error(
                "hdl_pin_mapping_invalid",
                format!("{pin_path}.name"),
                format!(
                    "HDL pin `{}` name must map to `port` or `port[bit]`",
                    pin.id.as_str()
                ),
            );
        } else if !mappings.insert(mapping.to_owned()) {
            report.error(
                "hdl_pin_mapping_duplicate",
                format!("{pin_path}.name"),
                format!("HDL port mapping `{mapping}` appears more than once"),
            );
        }
    }

    for key in component.parameters.keys() {
        if let Some(name) = key.strip_prefix("param.")
            && !is_hdl_identifier(name)
        {
            report.error(
                "hdl_parameter_name_invalid",
                format!("{component_path}.parameters.{key}"),
                format!("HDL parameter override `{name}` is not a safe identifier"),
            );
        }
    }
}

fn is_hdl_port_reference(value: &str) -> bool {
    if is_hdl_identifier(value) {
        return true;
    }
    let Some(open) = value.rfind('[') else {
        return false;
    };
    if !value.ends_with(']') || !is_hdl_identifier(&value[..open]) {
        return false;
    }
    value[open + 1..value.len() - 1].parse::<usize>().is_ok()
}

fn is_hdl_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| {
            character == '_' || character == '$' || character.is_ascii_alphanumeric()
        })
}

fn validate_mixed_signal_bridge(
    component: &Component,
    component_path: &str,
    report: &mut ValidationReport,
) {
    if component.kind == ComponentKind::adc_bridge() {
        validate_bridge_pin(
            component,
            component_path,
            "analog_in",
            SignalDomain::Analog,
            PinDirection::Input,
            report,
        );
        validate_bridge_pin(
            component,
            component_path,
            "digital_out",
            SignalDomain::Digital,
            PinDirection::Output,
            report,
        );
        let low = validate_bridge_quantity(
            component,
            component_path,
            "low_threshold",
            Unit::Volt,
            true,
            false,
            report,
        );
        let high = validate_bridge_quantity(
            component,
            component_path,
            "high_threshold",
            Unit::Volt,
            true,
            false,
            report,
        );
        if matches!((low, high), (Some(low), Some(high)) if low >= high) {
            report.error(
                "invalid_adc_threshold_window",
                format!("{component_path}.parameters"),
                "adc_bridge low_threshold must be strictly below high_threshold",
            );
        }
        for key in ["rise_delay", "fall_delay"] {
            validate_bridge_quantity(
                component,
                component_path,
                key,
                Unit::Second,
                false,
                true,
                report,
            );
        }
    } else if component.kind == ComponentKind::dac_bridge() {
        validate_bridge_pin(
            component,
            component_path,
            "digital_in",
            SignalDomain::Digital,
            PinDirection::Input,
            report,
        );
        validate_bridge_pin(
            component,
            component_path,
            "analog_out",
            SignalDomain::Analog,
            PinDirection::Output,
            report,
        );
        let low = validate_bridge_quantity(
            component,
            component_path,
            "low_voltage",
            Unit::Volt,
            true,
            false,
            report,
        );
        let high = validate_bridge_quantity(
            component,
            component_path,
            "high_voltage",
            Unit::Volt,
            true,
            false,
            report,
        );
        if matches!((low, high), (Some(low), Some(high)) if low >= high) {
            report.error(
                "invalid_dac_voltage_window",
                format!("{component_path}.parameters"),
                "dac_bridge low_voltage must be strictly below high_voltage",
            );
        }
        validate_bridge_quantity(
            component,
            component_path,
            "unknown_voltage",
            Unit::Volt,
            false,
            false,
            report,
        );
        for (key, unit) in [
            ("rise_time", Unit::Second),
            ("fall_time", Unit::Second),
            ("input_load", Unit::Farad),
        ] {
            validate_bridge_quantity(component, component_path, key, unit, false, true, report);
        }
    }
}

fn validate_bridge_pin(
    component: &Component,
    component_path: &str,
    pin_name: &str,
    expected_domain: SignalDomain,
    expected_direction: PinDirection,
    report: &mut ValidationReport,
) {
    let Some((index, pin)) = component
        .pins
        .iter()
        .enumerate()
        .find(|(_, pin)| pin.id.as_str() == pin_name)
    else {
        report.error(
            "mixed_signal_bridge_pin_missing",
            format!("{component_path}.pins"),
            format!("bridge requires pin `{pin_name}`"),
        );
        return;
    };
    if pin.domain != expected_domain {
        report.error(
            "mixed_signal_bridge_pin_domain",
            format!("{component_path}.pins[{index}].domain"),
            format!("pin `{pin_name}` must use {expected_domain:?} domain"),
        );
    }
    if pin.direction != expected_direction {
        report.error(
            "mixed_signal_bridge_pin_direction",
            format!("{component_path}.pins[{index}].direction"),
            format!("pin `{pin_name}` must use {expected_direction:?} direction"),
        );
    }
}

fn validate_bridge_quantity(
    component: &Component,
    component_path: &str,
    key: &str,
    expected_unit: Unit,
    required: bool,
    non_negative: bool,
    report: &mut ValidationReport,
) -> Option<f64> {
    let path = format!("{component_path}.parameters.{key}");
    let Some(value) = component.parameters.get(key) else {
        if required {
            report.error(
                "mixed_signal_bridge_parameter_missing",
                path,
                format!("bridge requires parameter `{key}`"),
            );
        }
        return None;
    };
    let number = match value {
        ParameterValue::Number(value) => *value,
        ParameterValue::Quantity(quantity) if quantity.unit == expected_unit => quantity.value,
        ParameterValue::Quantity(quantity) => {
            report.error(
                "mixed_signal_bridge_parameter_unit",
                path,
                format!(
                    "parameter `{key}` has unit {:?}, expected {expected_unit:?}",
                    quantity.unit
                ),
            );
            return None;
        }
        _ => {
            report.error(
                "mixed_signal_bridge_parameter_type",
                path,
                format!("parameter `{key}` must be numeric or an {expected_unit:?} quantity"),
            );
            return None;
        }
    };
    if !number.is_finite() {
        report.error(
            "mixed_signal_bridge_parameter_non_finite",
            path,
            format!("parameter `{key}` must be finite"),
        );
        return None;
    }
    if non_negative && number < 0.0 {
        report.error(
            "mixed_signal_bridge_parameter_negative",
            path,
            format!("parameter `{key}` must be non-negative"),
        );
        return None;
    }
    Some(number)
}
