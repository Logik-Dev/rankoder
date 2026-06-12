use tracing::{debug, instrument};

use crate::models::{
    media_file::MediaFile,
    transcode::{SkipReason, TranscodeDecision},
};

// With min_compression_potential=1.0 (default), the effective bpp thresholds become:
//   1080p (factor 1.5): bpp > min_bpp + 0.067  (~5.3 Mbps at 1080p24)
//   4K    (factor 3.0): bpp > min_bpp + 0.033  (~14.4 Mbps at 4K24)
const POTENTIAL_SCALE_FACTOR: f64 = 10.0;

#[derive(Clone)]
pub struct TakeTranscodeDecisionService {
    min_size_per_hour_gb: f64,
    min_bpp: f64,
    min_compression_potential: f64,
}

impl TakeTranscodeDecisionService {
    pub fn new(min_size_per_hour_gb: f64, min_bpp: f64, min_compression_potential: f64) -> Self {
        Self {
            min_size_per_hour_gb,
            min_bpp,
            min_compression_potential,
        }
    }

    #[instrument(skip(self, file), name = "decision", fields(id = ?file.id), ret)]
    pub fn execute(&self, file: &MediaFile, tmdb_rating: Option<f32>) -> TranscodeDecision {
        debug!("taking transcode decision");

        let Some(vp) = &file.video_properties else {
            return TranscodeDecision::Skip(SkipReason::MissingProbeData);
        };

        if !vp.video_codec.needs_transcoding() {
            return TranscodeDecision::Skip(SkipReason::ExcludedCodec);
        }

        let Some(duration) = &vp.duration else {
            return TranscodeDecision::Skip(SkipReason::MissingProbeData);
        };

        let size_per_hour_gb = vp.size_bytes.as_gb() / duration.as_hours_f64();
        if size_per_hour_gb < self.min_size_per_hour_gb {
            return TranscodeDecision::Skip(SkipReason::FileTooSmall);
        }

        let Some(bpp) = vp.bits_per_pixel() else {
            return TranscodeDecision::Skip(SkipReason::MissingProbeData);
        };

        if bpp < self.min_bpp {
            return TranscodeDecision::Skip(SkipReason::AlreadyCompressed);
        }

        let resolution_factor = resolution_factor(vp.resolution.height(), vp.resolution.width());
        let compression_potential =
            (bpp - self.min_bpp) * POTENTIAL_SCALE_FACTOR * resolution_factor as f64;

        if compression_potential <= self.min_compression_potential {
            return TranscodeDecision::SkipWithAnalysis {
                reason: SkipReason::InsufficientCompressionPotential,
                bpp,
                compression_potential,
            };
        }

        let crf = crf_from_rating_and_bpp(bpp, tmdb_rating);

        TranscodeDecision::Encode {
            bpp,
            compression_potential,
            crf,
        }
    }
}

pub fn crf_from_rating_and_bpp(bpp: f64, rating: Option<f32>) -> u8 {
    let base_crf: i8 = match rating {
        Some(r) if r >= 7.5 => 22,
        Some(r) if r >= 6.0 => 24,
        Some(r) if r >= 4.0 => 26,
        Some(_) => 28,
        None => 24,
    };

    let adjustment: i8 = match bpp {
        b if b >= 0.15 => -1,
        b if b >= 0.08 => 0,
        b if b >= 0.05 => 1,
        _ => 2,
    };

    (base_crf + adjustment).clamp(20, 30) as u8
}

fn resolution_factor(height: u32, width: u32) -> f32 {
    match height.saturating_mul(width) {
        p if p >= 3840 * 2160 => 3.0,
        p if p >= 1920 * 1080 => 1.5,
        p if p >= 1280 * 720 => 1.0,
        _ => 0.6,
    }
}
