use core::str;
use std::{convert::Infallible, fmt::Display, str::FromStr};

use crate::models::{error::DomainError, media_file::SizeBytes};

// VideoProperties
#[derive(Debug)]
pub struct VideoProperties {
    pub video_codec: VideoCodec,
    pub resolution: Resolution,
    pub bitrate: Option<Bitrate>,
    pub framerate: Option<Framerate>,
    pub size_bytes: SizeBytes,
}

impl VideoProperties {
    pub fn bits_per_pixel(&self) -> Option<f64> {
        let bitrate = self.bitrate.as_ref()?.as_bps() as f64;
        let pixels = self.resolution.pixel_count() as f64;
        let fps = self.framerate.as_ref()?.as_f64();

        Some(bitrate / (pixels * fps))
    }
}

// Resolution
#[derive(Debug)]
pub struct Resolution {
    height: u32,
    width: u32,
}

impl Resolution {
    pub fn new(height: u32, width: u32) -> Result<Self, DomainError> {
        if height == 0 || width == 0 {
            return Err(DomainError::InvalidResolution);
        }

        Ok(Self { height, width })
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn pixel_count(&self) -> u64 {
        self.height as u64 * self.width as u64
    }
}

// Codec
#[derive(Debug)]
pub enum VideoCodec {
    H264,
    Hevc,
    Av1,
    Missing,
    Unknown(String),
}

impl VideoCodec {
    pub fn needs_transcoding(&self) -> bool {
        match self {
            Self::H264 => true,
            Self::Hevc => false,
            Self::Av1 => false,
            Self::Missing => true,
            Self::Unknown(_) => true,
        }
    }
}

impl FromStr for VideoCodec {
    type Err = Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "h264" => Ok(Self::H264),
            "hevc" => Ok(Self::Hevc),
            "av1" => Ok(Self::Av1),
            other => Ok(Self::Unknown(other.to_string())),
        }
    }
}

impl AsRef<str> for VideoCodec {
    fn as_ref(&self) -> &str {
        match self {
            Self::H264 => "h264",
            Self::Hevc => "hevc",
            Self::Av1 => "av1",
            Self::Missing => "missing",
            Self::Unknown(other) => other,
        }
    }
}

// Bitrate
#[derive(Debug)]
pub struct Bitrate(u64);

impl Bitrate {
    pub fn new(value: u64) -> Result<Self, DomainError> {
        if value == 0 {
            return Err(DomainError::InvalidBitrate);
        }

        Ok(Self(value))
    }

    pub fn as_bps(&self) -> u64 {
        self.0
    }
}

// Framerate
#[derive(Debug)]
pub struct Framerate {
    numerator: u32,
    denominator: u32,
}

impl FromStr for Framerate {
    type Err = DomainError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (num, den) = s.split_once("/").ok_or(DomainError::InvalidFramerate)?;

        let numerator = num.parse().map_err(|_| DomainError::InvalidFramerate)?;
        let denominator = den.parse().map_err(|_| DomainError::InvalidFramerate)?;

        Self::new(numerator, denominator)
    }
}

impl Framerate {
    pub fn new(numerator: u32, denominator: u32) -> Result<Self, DomainError> {
        if denominator == 0 {
            return Err(DomainError::InvalidFramerate);
        }

        Ok(Self {
            numerator,
            denominator,
        })
    }

    pub fn as_f64(&self) -> f64 {
        self.numerator as f64 / self.denominator as f64
    }
}

impl Display for Framerate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.numerator, self.denominator)
    }
}
