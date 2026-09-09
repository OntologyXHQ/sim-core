use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{CIRCUIT_SCHEMA_VERSION, Quantity, model::ModelDefinition};

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self::new(value)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self::new(value)
            }
        }
    };
}

string_id!(ComponentId);
string_id!(PinId);
string_id!(NetId);

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ComponentKind(pub String);

impl ComponentKind {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn resistor() -> Self {
        Self::new("resistor")
    }
    pub fn capacitor() -> Self {
        Self::new("capacitor")
    }
    pub fn inductor() -> Self {
        Self::new("inductor")
    }
    pub fn voltage_source() -> Self {
        Self::new("voltage_source")
    }
    pub fn current_source() -> Self {
        Self::new("current_source")
    }
    pub fn ground() -> Self {
        Self::new("ground")
    }
    pub fn logic_gate() -> Self {
        Self::new("logic_gate")
    }
    pub fn diode() -> Self {
        Self::new("diode")
    }
    pub fn bjt() -> Self {
        Self::new("bjt")
    }
    pub fn mosfet() -> Self {
        Self::new("mosfet")
    }
    pub fn op_amp() -> Self {
        Self::new("op_amp")
    }
    pub fn subcircuit() -> Self {
        Self::new("subcircuit")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalDomain {
    Analog,
    Digital,
    Mixed,
    Reference,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinDirection {
    Input,
    Output,
    Bidirectional,
    Passive,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pin {
    pub id: PinId,
    pub name: String,
    pub domain: SignalDomain,
    pub direction: PinDirection,
}

impl Pin {
    pub fn new(
        id: impl Into<PinId>,
        name: impl Into<String>,
        domain: SignalDomain,
        direction: PinDirection,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            domain,
            direction,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ParameterValue {
    Boolean(bool),
    Integer(i64),
    Number(f64),
    Text(String),
    Quantity(Quantity),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Component {
    pub id: ComponentId,
    pub kind: ComponentKind,
    #[serde(default)]
    pub pins: Vec<Pin>,
    #[serde(default)]
    pub parameters: BTreeMap<String, ParameterValue>,
}

impl Component {
    pub fn new(id: impl Into<ComponentId>, kind: ComponentKind) -> Self {
        Self {
            id: id.into(),
            kind,
            pins: Vec::new(),
            parameters: BTreeMap::new(),
        }
    }

    pub fn with_pin(mut self, pin: Pin) -> Self {
        self.pins.push(pin);
        self
    }

    pub fn with_parameter(mut self, key: impl Into<String>, value: ParameterValue) -> Self {
        self.parameters.insert(key.into(), value);
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct NetEndpoint {
    pub component: ComponentId,
    pub pin: PinId,
}

impl NetEndpoint {
    pub fn new(component: impl Into<ComponentId>, pin: impl Into<PinId>) -> Self {
        Self {
            component: component.into(),
            pin: pin.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Net {
    pub id: NetId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub endpoints: Vec<NetEndpoint>,
}

impl Net {
    pub fn new(id: impl Into<NetId>) -> Self {
        Self {
            id: id.into(),
            label: None,
            endpoints: Vec::new(),
        }
    }

    pub fn connect(mut self, endpoint: NetEndpoint) -> Self {
        self.endpoints.push(endpoint);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Circuit {
    #[serde(default = "default_schema_version")]
    pub schema_version: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub components: Vec<Component>,
    #[serde(default)]
    pub nets: Vec<Net>,
    #[serde(default)]
    pub models: Vec<ModelDefinition>,
}

const fn default_schema_version() -> u16 {
    CIRCUIT_SCHEMA_VERSION
}

impl Circuit {
    pub fn new() -> Self {
        Self {
            schema_version: CIRCUIT_SCHEMA_VERSION,
            title: None,
            components: Vec::new(),
            nets: Vec::new(),
            models: Vec::new(),
        }
    }

    pub fn with_component(mut self, component: Component) -> Self {
        self.components.push(component);
        self
    }

    pub fn with_net(mut self, net: Net) -> Self {
        self.nets.push(net);
        self
    }

    pub fn with_model(mut self, model: ModelDefinition) -> Self {
        self.models.push(model);
        self
    }
}

impl Default for Circuit {
    fn default() -> Self {
        Self::new()
    }
}
