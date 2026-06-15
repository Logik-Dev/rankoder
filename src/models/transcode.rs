use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    ExcludedCodec,
    FileTooSmall,
    AlreadyCompressed,
    InsufficientCompressionPotential,
    MissingProbeData,
    InsufficientSizeReduction,
}

#[derive(Debug)]
pub enum TranscodeDecision {
    Encode {
        bpp: f64,
        compression_potential: f64,
        crf: u8,
    },
    SkipWithAnalysis {
        reason: SkipReason,
        bpp: f64,
        compression_potential: f64,
    },
    Skip(SkipReason),
}
