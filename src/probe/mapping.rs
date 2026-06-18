use std::str::FromStr;

use crate::{
    models::{
        media_file::SizeBytes,
        video::{
            Bitrate, ColorMetadata, DurationSecs, Framerate, MasterDisplay, MaxCll, Resolution,
            VideoCodec, VideoProperties,
        },
    },
    probe::{FfprobeOutput, error::FfprobeError, output::StreamType},
};

fn parse_fraction_num(s: &str, expected_denom: u32) -> Option<u32> {
    let (num, den) = s.split_once('/')?;
    if den.parse::<u32>().ok()? != expected_denom {
        return None;
    }
    num.parse::<u32>().ok()
}

impl TryFrom<FfprobeOutput> for VideoProperties {
    type Error = FfprobeError;
    fn try_from(value: FfprobeOutput) -> Result<Self, Self::Error> {
        let video_stream = value
            .streams
            .iter()
            .find(|s| matches!(s.codec_type, StreamType::Video) && !s.is_attached_pic())
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

        let duration = value
            .format
            .duration
            .as_deref()
            .and_then(|s| s.parse::<f64>().ok())
            .and_then(|secs| DurationSecs::new(secs).ok());

        let color_metadata = extract_color_metadata(video_stream);

        // Dolby Vision: the DOVI configuration record is a stream-level side
        // data, so it's already in our `-show_streams` output. Its presence
        // flags the file for skipping (a re-encode would strip the RPU).
        let dv_profile = video_stream
            .side_data_list
            .iter()
            .find(|sd| sd.side_data_type == "DOVI configuration record")
            .and_then(|sd| sd.dv_profile);

        Ok(Self {
            video_codec,
            resolution,
            bitrate,
            framerate,
            size_bytes,
            duration,
            color_metadata,
            dv_profile,
        })
    }
}

fn extract_color_metadata(
    video_stream: &crate::probe::output::FfprobeStream,
) -> Option<ColorMetadata> {
    let color_primaries = video_stream.color_primaries.clone()?;
    let color_trc = video_stream.color_transfer.clone()?;
    let colorspace = video_stream.color_space.clone()?;

    let mut master_display: Option<MasterDisplay> = None;
    let mut max_cll: Option<MaxCll> = None;

    for sd in &video_stream.side_data_list {
        match sd.side_data_type.as_str() {
            "Mastering display metadata" => {
                master_display = sd_to_master_display(sd);
            }
            "Content light level metadata" => {
                max_cll = Some(MaxCll {
                    max_content: sd.max_content?,
                    max_average: sd.max_average?,
                });
            }
            _ => {}
        }
    }

    Some(ColorMetadata {
        color_primaries,
        color_trc,
        colorspace,
        master_display: master_display.as_ref().map(|md| md.to_x265_string()),
        max_cll: max_cll.as_ref().map(|mc| mc.to_x265_string()),
    })
}

fn sd_to_master_display(sd: &crate::probe::output::SideData) -> Option<MasterDisplay> {
    Some(MasterDisplay {
        green: (
            parse_fraction_num(sd.green_x.as_deref()?, 50000)?,
            parse_fraction_num(sd.green_y.as_deref()?, 50000)?,
        ),
        blue: (
            parse_fraction_num(sd.blue_x.as_deref()?, 50000)?,
            parse_fraction_num(sd.blue_y.as_deref()?, 50000)?,
        ),
        red: (
            parse_fraction_num(sd.red_x.as_deref()?, 50000)?,
            parse_fraction_num(sd.red_y.as_deref()?, 50000)?,
        ),
        white_point: (
            parse_fraction_num(sd.white_point_x.as_deref()?, 50000)?,
            parse_fraction_num(sd.white_point_y.as_deref()?, 50000)?,
        ),
        luminance: (
            parse_fraction_num(sd.max_luminance.as_deref()?, 10000)?,
            parse_fraction_num(sd.min_luminance.as_deref()?, 10000)?,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hdr10_color_metadata_parsed() {
        let json = r#"{
            "streams": [
                {
                    "codec_type": "video",
                    "codec_name": "hevc",
                    "width": 3840,
                    "height": 2160,
                    "bit_rate": "25000000",
                    "avg_frame_rate": "24000/1001",
                    "color_primaries": "bt2020",
                    "color_transfer": "smpte2084",
                    "color_space": "bt2020nc",
                    "side_data_list": [
                        {
                            "side_data_type": "Mastering display metadata",
                            "red_x": "34000/50000",
                            "red_y": "16000/50000",
                            "green_x": "13250/50000",
                            "green_y": "34500/50000",
                            "blue_x": "7500/50000",
                            "blue_y": "3000/50000",
                            "white_point_x": "15635/50000",
                            "white_point_y": "16450/50000",
                            "max_luminance": "10000000/10000",
                            "min_luminance": "1/10000"
                        },
                        {
                            "side_data_type": "Content light level metadata",
                            "max_content": 1000,
                            "max_average": 400
                        }
                    ]
                }
            ],
            "format": {
                "duration": "3600.000000",
                "size": "50000000000",
                "bit_rate": "25000000"
            }
        }"#;

        let probe: FfprobeOutput = serde_json::from_str(json).unwrap();
        let vp: VideoProperties = probe.try_into().unwrap();

        assert_eq!(vp.resolution.width(), 3840);
        assert_eq!(vp.resolution.height(), 2160);

        let color = vp.color_metadata.unwrap();
        assert_eq!(color.color_primaries, "bt2020");
        assert_eq!(color.color_trc, "smpte2084");
        assert_eq!(color.colorspace, "bt2020nc");

        let md = color.master_display.unwrap();
        assert_eq!(
            md,
            "G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1)"
        );
        assert_eq!(color.max_cll.unwrap(), "1000,400");
    }

    #[test]
    fn dolby_vision_profile_detected() {
        let json = r#"{
            "streams": [
                {
                    "codec_type": "video",
                    "codec_name": "hevc",
                    "width": 3840,
                    "height": 2160,
                    "bit_rate": "30000000",
                    "avg_frame_rate": "24000/1001",
                    "side_data_list": [
                        {
                            "side_data_type": "DOVI configuration record",
                            "dv_profile": 8,
                            "dv_level": 6,
                            "rpu_present_flag": 1,
                            "el_present_flag": 0,
                            "bl_present_flag": 1
                        }
                    ]
                }
            ],
            "format": {
                "duration": "3600.000000",
                "size": "60000000000",
                "bit_rate": "30000000"
            }
        }"#;

        let probe: FfprobeOutput = serde_json::from_str(json).unwrap();
        let vp: VideoProperties = probe.try_into().unwrap();

        assert_eq!(vp.dv_profile, Some(8));
    }

    #[test]
    fn non_dv_has_no_profile() {
        let json = r#"{
            "streams": [
                {
                    "codec_type": "video",
                    "codec_name": "hevc",
                    "width": 1920,
                    "height": 1080,
                    "avg_frame_rate": "24000/1001"
                }
            ],
            "format": { "duration": "3600.0", "size": "10000000000" }
        }"#;

        let probe: FfprobeOutput = serde_json::from_str(json).unwrap();
        let vp: VideoProperties = probe.try_into().unwrap();

        assert_eq!(vp.dv_profile, None);
    }

    #[test]
    fn sdr_no_color_metadata() {
        let json = r#"{
            "streams": [
                {
                    "codec_type": "video",
                    "codec_name": "h264",
                    "width": 1920,
                    "height": 1080,
                    "avg_frame_rate": "24000/1001"
                }
            ],
            "format": {
                "duration": "3600.000000",
                "size": "10000000000"
            }
        }"#;

        let probe: FfprobeOutput = serde_json::from_str(json).unwrap();
        let vp: VideoProperties = probe.try_into().unwrap();

        assert!(vp.color_metadata.is_none());
    }

    #[test]
    fn attached_pic_stream_is_skipped() {
        let json = r#"{
            "streams": [
                {
                    "codec_type": "video",
                    "codec_name": "mjpeg",
                    "width": 600,
                    "height": 600,
                    "avg_frame_rate": "0/0",
                    "disposition": { "attached_pic": 1 }
                },
                {
                    "codec_type": "video",
                    "codec_name": "h264",
                    "width": 1920,
                    "height": 1080,
                    "avg_frame_rate": "24000/1001",
                    "disposition": { "attached_pic": 0 }
                }
            ],
            "format": {
                "duration": "3600.000000",
                "size": "10000000000"
            }
        }"#;

        let probe: FfprobeOutput = serde_json::from_str(json).unwrap();
        let vp: VideoProperties = probe.try_into().unwrap();

        assert!(matches!(vp.video_codec, VideoCodec::H264));
        assert_eq!(vp.resolution.width(), 1920);
        assert_eq!(vp.resolution.height(), 1080);
    }
}
