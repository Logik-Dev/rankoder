use std::str::FromStr;

use crate::{
    models::{
        common::AbsoluteFilePath,
        media_file::{MediaFile, SizeBytes},
        video::{
            Bitrate, ColorMetadata, DurationSecs, Framerate, Resolution, VideoCodec,
            VideoProperties,
        },
    },
    store::{dto::ColorMetadataRow, dto::MediaFileRow, error::StoreError},
};

fn color_md_from_row(row: &ColorMetadataRow) -> Option<ColorMetadata> {
    let cp = row.color_primaries.as_ref()?;
    let ct = row.color_trc.as_ref()?;
    let cs = row.colorspace.as_ref()?;
    Some(ColorMetadata {
        color_primaries: cp.clone(),
        color_trc: ct.clone(),
        colorspace: cs.clone(),
        master_display: row.master_display.clone(),
        max_cll: row.max_cll.clone(),
    })
}

impl TryFrom<(MediaFileRow, Option<ColorMetadataRow>)> for MediaFile {
    type Error = StoreError;

    fn try_from(
        (value, color_row): (MediaFileRow, Option<ColorMetadataRow>),
    ) -> Result<Self, Self::Error> {
        let id = value.id.into();
        let episode_id = value.episode_id.map(Into::into);
        let movie_id = value.movie_id.map(Into::into);
        let path = AbsoluteFilePath::new(value.file_path)?;

        let size_bytes = value
            .size_bytes
            .map(|s| SizeBytes::new(s as u64))
            .transpose()?;

        let video_codec = value
            .video_codec
            .as_deref()
            .map(|s| s.parse::<VideoCodec>().unwrap());

        let resolution = value
            .height
            .zip(value.width)
            .map(|(h, w)| Resolution::new(h as u32, w as u32))
            .transpose()?;

        let bitrate = value
            .bitrate_kbps
            .map(|b| Bitrate::new(b as u64))
            .transpose()?;

        let framerate = value
            .framerate
            .as_deref()
            .map(Framerate::from_str)
            .transpose()?;

        let duration = value
            .duration_seconds
            .and_then(|s| DurationSecs::new(s).ok());

        let color_metadata = color_row.as_ref().and_then(color_md_from_row);

        let dv_profile = value.dv_profile.map(|p| p as u8);

        let video_properties = match (video_codec, resolution, size_bytes) {
            (Some(video_codec), Some(resolution), Some(size_bytes)) => Some(VideoProperties {
                video_codec,
                resolution,
                size_bytes,
                bitrate,
                framerate,
                duration,
                color_metadata,
                dv_profile,
            }),
            _ => None,
        };

        Ok(Self {
            id,
            episode_id,
            movie_id,
            path,
            video_properties,
            transcode_spec: value.transcode_spec,
            workflow_state: value.workflow_state,
        })
    }
}
