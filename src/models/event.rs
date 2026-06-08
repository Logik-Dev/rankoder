use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MediaEvent {
    Discovered { source: String },
    Probed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovered_serializes_with_snake_case_type_tag() {
        let event = MediaEvent::Discovered {
            source: "jellyfin".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "type": "discovered",
                "source": "jellyfin"
            })
        );
    }

    #[test]
    fn discovered_round_trips() {
        let event = MediaEvent::Discovered {
            source: "jellyfin".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: MediaEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }
}
