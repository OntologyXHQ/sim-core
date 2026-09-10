use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisKind {
    OperatingPoint,
    DcSweep,
    Transient,
    AcSweep,
    DigitalTransient,
    MixedSignalTransient,
    FirmwareTransient,
    CoSimulationTransient,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisDomain {
    Analog,
    Digital,
    Mixed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcScale {
    Linear,
    Decade,
    Octave,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Analysis {
    OperatingPoint,
    DcSweep {
        source: String,
        start: f64,
        stop: f64,
        step: f64,
    },
    Transient {
        step: f64,
        stop: f64,
    },
    AcSweep {
        scale: AcScale,
        points: u32,
        start_hz: f64,
        stop_hz: f64,
    },
    DigitalTransient {
        stop: f64,
    },
    MixedSignalTransient {
        step: f64,
        stop: f64,
    },
    FirmwareTransient {
        step: f64,
        stop: f64,
    },
}

impl Analysis {
    pub const fn kind(&self) -> AnalysisKind {
        match self {
            Self::OperatingPoint => AnalysisKind::OperatingPoint,
            Self::DcSweep { .. } => AnalysisKind::DcSweep,
            Self::Transient { .. } => AnalysisKind::Transient,
            Self::AcSweep { .. } => AnalysisKind::AcSweep,
            Self::DigitalTransient { .. } => AnalysisKind::DigitalTransient,
            Self::MixedSignalTransient { .. } => AnalysisKind::MixedSignalTransient,
            Self::FirmwareTransient { .. } => AnalysisKind::FirmwareTransient,
        }
    }

    pub const fn domain(&self) -> AnalysisDomain {
        match self {
            Self::DigitalTransient { .. } => AnalysisDomain::Digital,
            Self::MixedSignalTransient { .. } => AnalysisDomain::Mixed,
            Self::FirmwareTransient { .. } => AnalysisDomain::Digital,
            _ => AnalysisDomain::Analog,
        }
    }
}
