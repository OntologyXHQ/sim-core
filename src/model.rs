use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelId(pub String);

impl ModelId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ModelId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for ModelId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelLanguage {
    Spice,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    Device,
    Subcircuit,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModelDefinition {
    pub id: ModelId,
    pub language: ModelLanguage,
    pub kind: ModelKind,
    /// Symbol used by the simulator when instantiating this model, for example
    /// `1N4148` for `.model 1N4148 D (...)` or `LM358` for `.subckt LM358 ...`.
    pub entry: String,
    /// Inline model source. Engine adapters validate this source before it is
    /// admitted to an isolated simulator process.
    pub source: String,
}

impl ModelDefinition {
    pub fn spice_device(
        id: impl Into<ModelId>,
        entry: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            language: ModelLanguage::Spice,
            kind: ModelKind::Device,
            entry: entry.into(),
            source: source.into(),
        }
    }

    pub fn spice_subcircuit(
        id: impl Into<ModelId>,
        entry: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            language: ModelLanguage::Spice,
            kind: ModelKind::Subcircuit,
            entry: entry.into(),
            source: source.into(),
        }
    }
}
