use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Volt,
    Ampere,
    Ohm,
    Farad,
    Henry,
    Hertz,
    Second,
    Siemens,
    Watt,
    Dimensionless,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Quantity {
    pub value: f64,
    pub unit: Unit,
}

impl Quantity {
    pub const fn new(value: f64, unit: Unit) -> Self {
        Self { value, unit }
    }

    pub fn is_finite(self) -> bool {
        self.value.is_finite()
    }
}
