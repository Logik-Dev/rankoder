#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoder {
    Nvenc,
    VideoToolbox,
    Libx265,
}

impl Encoder {
    pub fn from_env_override() -> Option<Self> {
        let val = std::env::var("TRANSCODE_ENCODER").ok()?;
        match val.as_str() {
            "nvenc" => Some(Self::Nvenc),
            "videotoolbox" => Some(Self::VideoToolbox),
            "libx265" => Some(Self::Libx265),
            _ => None,
        }
    }

    pub fn build_args(&self, crf: u8) -> Vec<String> {
        let mut args: Vec<String> = vec!["-map".into(), "0".into(), "-c".into(), "copy".into()];
        match self {
            Self::Nvenc => {
                args.extend([
                    "-c:v".into(),
                    "hevc_nvenc".into(),
                    "-pix_fmt".into(),
                    "p010le".into(),
                    "-profile:v".into(),
                    "main10".into(),
                    "-preset".into(),
                    "p7".into(),
                    "-tune".into(),
                    "hq".into(),
                    "-rc".into(),
                    "vbr".into(),
                    "-cq".into(),
                    crf.to_string(),
                    "-b:v".into(),
                    "0".into(),
                    "-spatial-aq".into(),
                    "1".into(),
                    "-aq-strength".into(),
                    "8".into(),
                    "-rc-lookahead".into(),
                    "32".into(),
                    "-bf".into(),
                    "0".into(),
                    "-tag:v".into(),
                    "hvc1".into(),
                ]);
            }
            Self::Libx265 => {
                args.extend([
                    "-c:v".into(),
                    "libx265".into(),
                    "-pix_fmt".into(),
                    "yuv420p10le".into(),
                    "-profile:v".into(),
                    "main10".into(),
                    "-preset".into(),
                    "slow".into(),
                    "-crf".into(),
                    crf.to_string(),
                    "-x265-params".into(),
                    "aq-mode=3".into(),
                    "-tag:v".into(),
                    "hvc1".into(),
                ]);
            }
            Self::VideoToolbox => {
                let q = 100u16.saturating_sub(crf as u16 * 2);
                args.extend([
                    "-c:v".into(),
                    "hevc_videotoolbox".into(),
                    "-pix_fmt".into(),
                    "p010le".into(),
                    "-profile:v".into(),
                    "main10".into(),
                    "-q:v".into(),
                    q.to_string(),
                    "-tag:v".into(),
                    "hvc1".into(),
                ]);
            }
        }
        args
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvenc_build_args() {
        let args = Encoder::Nvenc.build_args(23);
        assert!(args.contains(&"-c:v".into()));
        assert!(args.contains(&"hevc_nvenc".into()));
        assert!(args.contains(&"-pix_fmt".into()));
        assert!(args.contains(&"p010le".into()));
        assert!(args.contains(&"-profile:v".into()));
        assert!(args.contains(&"main10".into()));
        assert!(args.contains(&"-preset".into()));
        assert!(args.contains(&"p7".into()));
        assert!(args.contains(&"-tune".into()));
        assert!(args.contains(&"hq".into()));
        assert!(args.contains(&"-rc".into()));
        assert!(args.contains(&"vbr".into()));
        assert!(args.contains(&"-cq".into()));
        assert!(args.contains(&"23".into()));
        assert!(args.contains(&"-b:v".into()));
        assert!(args.contains(&"0".into()));
        assert!(args.contains(&"-spatial-aq".into()));
        assert!(args.contains(&"1".into()));
        assert!(args.contains(&"-aq-strength".into()));
        assert!(args.contains(&"8".into()));
        assert!(args.contains(&"-rc-lookahead".into()));
        assert!(args.contains(&"32".into()));
        assert!(args.contains(&"-bf".into()));
        assert!(args.contains(&"0".into()));
        assert!(args.contains(&"-tag:v".into()));
        assert!(args.contains(&"hvc1".into()));
    }

    #[test]
    fn libx265_build_args() {
        let args = Encoder::Libx265.build_args(28);
        assert!(args.contains(&"-c:v".into()));
        assert!(args.contains(&"libx265".into()));
        assert!(args.contains(&"-pix_fmt".into()));
        assert!(args.contains(&"yuv420p10le".into()));
        assert!(args.contains(&"-profile:v".into()));
        assert!(args.contains(&"main10".into()));
        assert!(args.contains(&"-preset".into()));
        assert!(args.contains(&"slow".into()));
        assert!(args.contains(&"-crf".into()));
        assert!(args.contains(&"28".into()));
        assert!(args.contains(&"-x265-params".into()));
        assert!(args.contains(&"aq-mode=3".into()));
        assert!(args.contains(&"-tag:v".into()));
        assert!(args.contains(&"hvc1".into()));
    }

    #[test]
    fn videotoolbox_q_mapping() {
        let args = Encoder::VideoToolbox.build_args(23);
        assert!(args.contains(&"-q:v".into()));
        let pos = args.iter().position(|a| a == "-q:v").unwrap();
        let q_val = &args[pos + 1];
        assert_eq!(q_val, "54"); // 100 - 23*2 = 54
    }

    #[test]
    fn videotoolbox_min_q_clamped() {
        let args = Encoder::VideoToolbox.build_args(50);
        let pos = args.iter().position(|a| a == "-q:v").unwrap();
        let q_val = &args[pos + 1];
        assert_eq!(q_val, "0"); // 100 - 100 = 0, saturating_sub keeps it at 0
    }

    #[test]
    fn base_prefix_present_in_all_args() {
        for enc in [Encoder::Nvenc, Encoder::VideoToolbox, Encoder::Libx265] {
            let args = enc.build_args(20);
            assert!(args.contains(&"-map".into()));
            assert!(args.contains(&"0".into()));
            assert!(args.contains(&"-c".into()));
            assert!(args.contains(&"copy".into()));
        }
    }
}
