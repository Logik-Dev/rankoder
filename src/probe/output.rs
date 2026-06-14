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
    pub color_primaries: Option<String>,
    pub color_transfer: Option<String>,
    pub color_space: Option<String>,
    #[serde(default)]
    pub side_data_list: Vec<SideData>,
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
    pub duration: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct SideData {
    pub side_data_type: String,
    // Mastering display
    pub red_x: Option<String>,
    pub red_y: Option<String>,
    pub green_x: Option<String>,
    pub green_y: Option<String>,
    pub blue_x: Option<String>,
    pub blue_y: Option<String>,
    pub white_point_x: Option<String>,
    pub white_point_y: Option<String>,
    pub max_luminance: Option<String>,
    pub min_luminance: Option<String>,
    // Content light level
    pub max_content: Option<u16>,
    pub max_average: Option<u16>,
}
