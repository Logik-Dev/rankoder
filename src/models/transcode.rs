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
    QualityTooLow,
    DolbyVision,
}

#[derive(Debug)]
pub enum TranscodeDecision {
    Encode {
        bpp: f64,
        compression_potential: f64,
        crf: u8,
        /// Estimated fraction of the file size we expect to reclaim, in [0, 1).
        /// Used only to populate the approval request; the real saving is
        /// measured after transcoding. Not a decision input.
        estimated_saving_ratio: f64,
    },
    SkipWithAnalysis {
        reason: SkipReason,
        bpp: f64,
        compression_potential: f64,
    },
    Skip(SkipReason),
}
