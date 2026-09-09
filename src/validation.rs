use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    CIRCUIT_SCHEMA_VERSION, Circuit, ComponentKind, ModelKind, ParameterValue, PinDirection,
    SignalDomain,
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
        } else if models.insert(model_id, model.kind).is_some() {
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
                        Some(actual_kind) if *actual_kind != expected_model_kind => report.error(
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

        if domains.contains(&SignalDomain::Analog)
            && domains.contains(&SignalDomain::Digital)
            && !domains.contains(&SignalDomain::Mixed)
        {
            report.error(
                "mixed_signal_bridge_required",
                format!("{net_path}.endpoints"),
                "analog and digital pins cannot share a net without an explicit mixed-signal bridge",
            );
        }
    }

    report
}
