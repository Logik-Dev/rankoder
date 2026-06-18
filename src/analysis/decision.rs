use tracing::{debug, instrument};

use crate::models::{
    media_file::MediaFile,
    transcode::{SkipReason, TranscodeDecision},
    video::VideoCodec,
};

// With min_compression_potential=1.0 (default), the effective bpp thresholds become:
//   1080p (factor 1.5): bpp > min_bpp + 0.067  (~5.3 Mbps at 1080p24)
//   4K    (factor 3.0): bpp > min_bpp + 0.033  (~14.4 Mbps at 4K24)
const POTENTIAL_SCALE_FACTOR: f64 = 10.0;

#[derive(Clone)]
pub struct TakeTranscodeDecisionService {
    min_size_per_hour_gb: f64,
    min_bpp: f64,
    min_bpp_hevc: f64,
    min_compression_potential: f64,
}

impl TakeTranscodeDecisionService {
    pub fn new(
        min_size_per_hour_gb: f64,
        min_bpp: f64,
        min_bpp_hevc: f64,
        min_compression_potential: f64,
    ) -> Self {
        Self {
            min_size_per_hour_gb,
            min_bpp,
            min_bpp_hevc,
            min_compression_potential,
        }
    }

    #[instrument(skip(self, file), name = "decision", fields(id = ?file.id))]
    pub fn execute(&self, file: &MediaFile, tmdb_rating: Option<f32>) -> TranscodeDecision {
        debug!("taking transcode decision");

        let Some(vp) = &file.video_properties else {
            return TranscodeDecision::Skip(SkipReason::MissingProbeData);
        };

        // Dolby Vision first: a normal re-encode strips the DV RPU and degrades
        // playback (washed-out/wrong colors, especially profile 5), so DV is
        // never transcoded until proper RPU handling exists. Checked before the
        // codec/bpp gates so the skip reason is always reported as DolbyVision.
        if let Some(profile) = vp.dv_profile {
            debug!(profile, "skipping Dolby Vision file");
            return TranscodeDecision::Skip(SkipReason::DolbyVision);
        }

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

        // HEVC is already efficient, so it gets a higher baseline: only clearly
        // over-bitrate (remux-tier) sources qualify. Everything downstream — the
        // compression_potential gate and the saving estimate — uses this
        // baseline, so the resolution-aware headroom check applies uniformly.
        let effective_min_bpp = if matches!(vp.video_codec, VideoCodec::Hevc) {
            self.min_bpp_hevc
        } else {
            self.min_bpp
        };

        if bpp < effective_min_bpp {
            return TranscodeDecision::Skip(SkipReason::AlreadyCompressed);
        }

        let resolution_factor = resolution_factor(vp.resolution.height(), vp.resolution.width());
        let compression_potential =
            (bpp - effective_min_bpp) * POTENTIAL_SCALE_FACTOR * resolution_factor as f64;

        if compression_potential <= self.min_compression_potential {
            return TranscodeDecision::SkipWithAnalysis {
                reason: SkipReason::InsufficientCompressionPotential,
                bpp,
                compression_potential,
            };
        }

        let crf = crf_from_rating_and_bpp(bpp, tmdb_rating);

        // Estimated reclaimed fraction: how much of the current bitrate sits
        // above the minimum acceptable bpp. bpp > effective_min_bpp is
        // guaranteed here (checked above), so this is in (0, 1).
        let estimated_saving_ratio = ((bpp - effective_min_bpp) / bpp).clamp(0.0, 1.0);

        TranscodeDecision::Encode {
            bpp,
            compression_potential,
            crf,
            estimated_saving_ratio,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{
        common::AbsoluteFilePath,
        media_file::{MediaFile, MediaFileId, SizeBytes},
        video::{Resolution, VideoProperties},
        workflow::WorkflowStateTag,
    };

    fn file_with(dv_profile: Option<u8>, codec: &str) -> MediaFile {
        MediaFile {
            id: MediaFileId::new(),
            episode_id: None,
            movie_id: None,
            path: AbsoluteFilePath::new("/tmp/dv_test.mkv").unwrap(),
            video_properties: Some(VideoProperties {
                video_codec: codec.parse().unwrap(),
                resolution: Resolution::new(2160, 3840).unwrap(),
                bitrate: None,
                framerate: None,
                size_bytes: SizeBytes::new(50_000_000_000).unwrap(),
                duration: None,
                color_metadata: None,
                dv_profile,
            }),
            transcode_spec: None,
            workflow_state: WorkflowStateTag::Probed,
        }
    }

    fn service() -> TakeTranscodeDecisionService {
        TakeTranscodeDecisionService::new(2.0, 0.04, 0.15, 1.0)
    }

    #[test]
    fn dolby_vision_is_skipped() {
        let decision = service().execute(&file_with(Some(5), "hevc"), None);
        assert!(matches!(
            decision,
            TranscodeDecision::Skip(SkipReason::DolbyVision)
        ));
    }

    #[test]
    fn dolby_vision_skip_precedes_excluded_codec() {
        // AV1 alone would be ExcludedCodec, but DV must win so the affected
        // population is counted accurately.
        let decision = service().execute(&file_with(Some(5), "av1"), None);
        assert!(matches!(
            decision,
            TranscodeDecision::Skip(SkipReason::DolbyVision)
        ));
    }
}
