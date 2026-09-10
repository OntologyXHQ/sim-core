use serde::{Deserialize, Serialize};

use crate::EngineError;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FirmwareFormat {
    Elf,
    IntelHex,
    Binary,
}

/// Immutable firmware payload handed to an MCU backend.
///
/// The core intentionally does not parse or emulate executable formats. Renode owns
/// ELF/HEX/BIN loading; sim-core only keeps the bytes and the minimal metadata needed
/// to invoke that backend deterministically.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FirmwareArtifact {
    pub format: FirmwareFormat,
    pub bytes: Vec<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_address: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_point: Option<u64>,
}

impl FirmwareArtifact {
    pub fn elf(bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            format: FirmwareFormat::Elf,
            bytes: bytes.into(),
            load_address: None,
            entry_point: None,
        }
    }

    pub fn intel_hex(bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            format: FirmwareFormat::IntelHex,
            bytes: bytes.into(),
            load_address: None,
            entry_point: None,
        }
    }

    pub fn binary(bytes: impl Into<Vec<u8>>, load_address: u64) -> Self {
        Self {
            format: FirmwareFormat::Binary,
            bytes: bytes.into(),
            load_address: Some(load_address),
            entry_point: None,
        }
    }

    pub fn with_entry_point(mut self, entry_point: u64) -> Self {
        self.entry_point = Some(entry_point);
        self
    }

    pub fn validate(&self) -> Result<(), EngineError> {
        if self.bytes.is_empty() {
            return Err(EngineError::new(
                "firmware_artifact_invalid",
                "firmware artifact must not be empty",
            ));
        }
        if self.format == FirmwareFormat::Binary && self.load_address.is_none() {
            return Err(EngineError::new(
                "firmware_artifact_invalid",
                "raw binary firmware requires an explicit load_address",
            ));
        }
        Ok(())
    }

    pub const fn extension(&self) -> &'static str {
        match self.format {
            FirmwareFormat::Elf => "elf",
            FirmwareFormat::IntelHex => "hex",
            FirmwareFormat::Binary => "bin",
        }
    }
}
