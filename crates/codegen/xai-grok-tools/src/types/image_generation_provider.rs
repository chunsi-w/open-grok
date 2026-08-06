use serde::{Deserialize, Serialize};

/// User-selected image generation service.
///
/// This is a routing decision only. Each provider keeps its own endpoint,
/// credentials, headers, and retry path.
///
/// Lives in `xai-grok-tools` (not `xai-grok-config-types`) because the tool
/// clients match on it: `xai-grok-config-types` transitively depends on
/// `xai-grok-tools` via `xai-grok-announcements`, so hosting it there is a
/// dependency cycle. `xai-grok-shared::ui_config` re-exports it so
/// `UiConfig`'s wire format is unchanged.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImageGenerationProvider {
    #[default]
    Grok,
    #[serde(rename = "openai")]
    OpenAi,
}

impl ImageGenerationProvider {
    pub const fn as_canonical(self) -> &'static str {
        match self {
            Self::Grok => "grok",
            Self::OpenAi => "openai",
        }
    }

    pub fn from_canonical(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "grok" => Some(Self::Grok),
            "openai" => Some(Self::OpenAi),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_strings_round_trip() {
        assert_eq!(ImageGenerationProvider::Grok.as_canonical(), "grok");
        assert_eq!(ImageGenerationProvider::OpenAi.as_canonical(), "openai");
        assert_eq!(
            ImageGenerationProvider::from_canonical("grok"),
            Some(ImageGenerationProvider::Grok)
        );
        assert_eq!(
            ImageGenerationProvider::from_canonical(" OpenAI "),
            Some(ImageGenerationProvider::OpenAi)
        );
        assert_eq!(ImageGenerationProvider::from_canonical("imagine"), None);
    }

    #[test]
    fn serde_uses_snake_case_wire_names() {
        assert_eq!(
            serde_json::to_string(&ImageGenerationProvider::OpenAi).unwrap(),
            r#""openai""#
        );
        assert_eq!(
            serde_json::from_str::<ImageGenerationProvider>(r#""grok""#).unwrap(),
            ImageGenerationProvider::Grok
        );
        assert!(serde_json::from_str::<ImageGenerationProvider>(r#""unknown""#).is_err());
    }
}
