use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

pub fn lenient<'de, D>(deserializer: D) -> Result<Option<Effort>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(raw.and_then(|value| serde_json::from_value(value).ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Effort; 5] = [
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::Xhigh,
        Effort::Max,
    ];

    #[test]
    fn the_wire_name_is_the_cli_name() {
        for effort in ALL {
            let wire = serde_json::to_value(effort).expect("serializing");
            assert_eq!(wire, serde_json::json!(effort.as_str()));
            let back: Effort = serde_json::from_value(wire).expect("deserializing");
            assert_eq!(back, effort);
        }
    }

    #[test]
    fn an_unknown_level_is_rejected() {
        assert!(serde_json::from_value::<Effort>(serde_json::json!("ultra")).is_err());
        assert!(serde_json::from_value::<Effort>(serde_json::json!("High")).is_err());
    }

    #[derive(Deserialize)]
    struct Holder {
        #[serde(default, deserialize_with = "lenient")]
        effort: Option<Effort>,
    }

    fn parse(json: serde_json::Value) -> Option<Effort> {
        serde_json::from_value::<Holder>(json)
            .expect("lenient never fails")
            .effort
    }

    #[test]
    fn lenient_keeps_known_levels() {
        assert_eq!(
            parse(serde_json::json!({"effort": "max"})),
            Some(Effort::Max)
        );
    }

    #[test]
    fn lenient_drops_unknown_null_and_missing() {
        assert_eq!(parse(serde_json::json!({"effort": "ultra"})), None);
        assert_eq!(parse(serde_json::json!({"effort": 3})), None);
        assert_eq!(parse(serde_json::json!({"effort": null})), None);
        assert_eq!(parse(serde_json::json!({})), None);
    }
}
