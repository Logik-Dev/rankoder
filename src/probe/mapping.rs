use std::str::FromStr;

use crate::{
    models::{
        media_file::SizeBytes,
        video::{Bitrate, Framerate, Resolution, VideoCodec, VideoProperties},
    },
    probe::{error::FfprobeError, output::StreamType, FfprobeOutput},
};

impl TryFrom<FfprobeOutput> for VideoProperties {
    type Error = FfprobeError;
    fn try_from(value: FfprobeOutput) -> Result<Self, Self::Error> {
        let video_stream = value
            .streams
            .iter()
            .find(|s| matches!(s.codec_type, StreamType::Video))
            .ok_or(FfprobeError::NoVideoStream)?;

        let video_codec = video_stream
            .codec_name
            .as_deref()
            .map(VideoCodec::from_str)
            .map(Result::unwrap)
            .unwrap_or(VideoCodec::Missing);

        let resolution = Resolution::new(
            video_stream.height.ok_or(FfprobeError::MissingResolution)?,
            video_stream.width.ok_or(FfprobeError::MissingResolution)?,
        )?;

        let bitrate = video_stream
            .bit_rate
            .as_deref()
            .or(value.format.bit_rate.as_deref())
            .and_then(|s| s.parse::<u64>().ok())
            .and_then(|b| Bitrate::new(b).ok());

        let framerate = video_stream
            .avg_frame_rate
            .as_deref()
            .and_then(|s| Framerate::from_str(s).ok());

        let size_bytes = value
            .format
            .size
            .ok_or(FfprobeError::MissingSizeBytes)
            .and_then(|s| s.parse::<u64>().map_err(|_| FfprobeError::MissingSizeBytes))
            .and_then(|v| SizeBytes::new(v).map_err(FfprobeError::from))?;

        Ok(Self {
            video_codec,
            resolution,
            bitrate,
            framerate,
            size_bytes,
        })
    }
}
