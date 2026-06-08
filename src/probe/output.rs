use serde::Deserialize;

#[derive(Deserialize)]
pub(super) struct FfprobeOutput {
    pub streams: Vec<FfprobeStream>,
    pub format: FfprobeFormat,
}

#[derive(Deserialize)]
pub(super) struct FfprobeStream {
    pub codec_type: StreamType,
    pub codec_name: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub bit_rate: Option<String>,
    pub avg_frame_rate: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum StreamType {
    Audio,
    Video,
    Subtitle,
    Data,
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
pub(super) struct FfprobeFormat {
    pub bit_rate: Option<String>,
    pub size: Option<String>,
}
