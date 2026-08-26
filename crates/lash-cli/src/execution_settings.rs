//! Host-owned execution-mode and RLM dialect settings.

pub use lash_rlm_types::RlmDialect;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    #[default]
    Standard,
    Rlm,
}

impl ExecutionMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Rlm => "rlm",
        }
    }

    pub const fn is_standard(self) -> bool {
        matches!(self, Self::Standard)
    }

    pub const fn is_rlm(self) -> bool {
        matches!(self, Self::Rlm)
    }
}

pub fn parse_execution_mode(input: &str) -> Result<ExecutionMode, String> {
    match input.trim().to_ascii_lowercase().as_str() {
        "" => Err("Execution mode cannot be empty.".to_string()),
        "rlm" => Ok(ExecutionMode::Rlm),
        "standard" | "tools" => Ok(ExecutionMode::Standard),
        other => Err(format!(
            "Unknown execution mode `{other}`. Expected `rlm` or `standard`."
        )),
    }
}

pub fn parse_rlm_dialect(input: &str) -> Result<RlmDialect, String> {
    let language_id = input.trim().to_ascii_lowercase();
    RlmDialect::from_language_id(&language_id).ok_or_else(|| {
        format!(
            "Unknown RLM dialect `{input}`. Expected {}.",
            RlmDialect::registered_language_ids()
        )
    })
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum RlmTerminationMode {
    #[value(name = "natural")]
    Natural,
    #[value(name = "finish-required")]
    FinishRequired,
}

impl RlmTerminationMode {
    pub fn as_rlm_termination(self) -> lash_rlm_types::RlmTermination {
        match self {
            Self::Natural => lash_rlm_types::RlmTermination::Natural,
            Self::FinishRequired => lash_rlm_types::RlmTermination::FinishRequired { schema: None },
        }
    }
}

pub fn default_rlm_termination_for_mode(mode: ExecutionMode) -> Option<RlmTerminationMode> {
    mode.is_rlm().then_some(RlmTerminationMode::Natural)
}

pub const fn execution_mode_usage() -> &'static str {
    "<rlm|standard>"
}

pub fn ensure_supported_execution_mode(mode: ExecutionMode) -> Result<ExecutionMode, String> {
    Ok(mode)
}

pub const fn execution_mode_label(mode: &ExecutionMode) -> &str {
    mode.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_registered_rlm_dialects() {
        assert_eq!(parse_rlm_dialect("lashlang"), Ok(RlmDialect::Lashlang));
        assert_eq!(parse_rlm_dialect("typescript"), Ok(RlmDialect::Typescript));
        let error = parse_rlm_dialect("python").expect_err("unregistered dialect");
        assert!(error.contains("`lashlang`"));
        assert!(error.contains("`typescript`"));
    }

    #[test]
    fn rlm_termination_mode_maps_to_protocol_termination() {
        assert_eq!(
            RlmTerminationMode::Natural.as_rlm_termination(),
            lash_rlm_types::RlmTermination::Natural
        );
        assert!(matches!(
            RlmTerminationMode::FinishRequired.as_rlm_termination(),
            lash_rlm_types::RlmTermination::FinishRequired { schema: None }
        ));
    }
}
