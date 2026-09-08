use sha2::{Digest, Sha256};
use std::io::IsTerminal;

pub const MIN_VISIBLE_DURATION_MS: u64 = 1_000;

/// First 12 hex chars of the SHA-256 of `bytes`; used for compact
/// toolset-identity hashes.
pub(crate) fn hash12(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("{:x}", digest)[..12].to_string()
}

/// Format a duration in milliseconds as a human-readable string.
///
/// - `< 1s` → "42ms"
/// - `1s – 60s` → "14.6s"
/// - `1m – 60m` → "2m 13s"
/// - `≥ 1h` → "1h 5m"
pub fn format_duration_ms(ms: u64) -> String {
    if ms < 1_000 {
        format!("{}ms", ms)
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1_000.0)
    } else if ms < 3_600_000 {
        let m = ms / 60_000;
        let s = (ms % 60_000) / 1_000;
        format!("{}m {}s", m, s)
    } else {
        let h = ms / 3_600_000;
        let m = (ms % 3_600_000) / 60_000;
        format!("{}h {}m", h, m)
    }
}

pub fn format_duration_ms_if_visible(ms: u64) -> Option<String> {
    (ms >= MIN_VISIBLE_DURATION_MS).then(|| format_duration_ms(ms))
}

pub fn manual_interrupt_message() -> &'static str {
    "Manually interrupted."
}

pub(crate) fn require_interactive_terminal(context: &str) -> anyhow::Result<()> {
    if std::io::stdin().is_terminal() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "{context} requires an interactive terminal (stdin is not a TTY)"
        ))
    }
}

pub fn is_cancelled_error(message: &str, code: Option<&str>) -> bool {
    if code.is_some_and(|code| matches!(code, "cancelled" | "canceled")) {
        return true;
    }

    let normalized = message
        .trim()
        .trim_end_matches(['.', '!', '?'])
        .to_ascii_lowercase();

    matches!(
        normalized.as_str(),
        "cancelled" | "canceled" | "llm error: cancelled" | "llm error: canceled"
    )
}

/// Compact settlement diagnostics shared by terminal modes. Evidence usage has
/// no presence marker upstream: a zero value may be reported or defaulted.
pub(crate) fn turn_diagnostic_lines(turn: &lash::TurnReport) -> Vec<String> {
    let mut lines: Vec<_> = turn
        .errors
        .iter()
        .map(|issue| {
            let label = match issue.severity {
                lash::turn::TurnIssueSeverity::Advisory => "warning",
                lash::turn::TurnIssueSeverity::Blocking => "error",
            };
            format!("{label}: {}", issue.message)
        })
        .collect();
    if matches!(turn.outcome, lash::TurnOutcome::Stopped(_)) {
        for evidence in &turn.failure_evidence {
            let usage = &evidence.billed_usage;
            let tokens = [
                usage.input_tokens,
                usage.output_tokens,
                usage.cache_read_input_tokens,
                usage.cache_write_input_tokens,
            ]
            .into_iter()
            .fold(0u64, |sum, value| sum.saturating_add(value.max(0) as u64));
            let usage_label = if tokens == 0 {
                "0 (reported or defaulted; usage may be unavailable)".to_string()
            } else {
                tokens.to_string()
            };
            lines.push(format!(
                "{}: billed tokens {}; partial output {}",
                evidence.refusal.code,
                usage_label,
                if evidence.partial_output.is_some() {
                    "present"
                } else {
                    "absent"
                }
            ));
        }
    }
    if let Some(omitted) = &turn.omitted {
        lines.push(format!(
            "Tool records omitted: {} ({} failures, {} attachments)",
            omitted.count,
            omitted.failures,
            omitted.attachments.len()
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::{format_duration_ms, format_duration_ms_if_visible};

    #[test]
    fn duration_under_visibility_threshold_is_hidden() {
        assert_eq!(format_duration_ms_if_visible(999), None);
    }

    #[test]
    fn duration_at_visibility_threshold_is_shown() {
        assert_eq!(
            format_duration_ms_if_visible(1_000),
            Some("1.0s".to_string())
        );
    }

    #[test]
    fn duration_formatter_still_formats_subsecond_values() {
        assert_eq!(format_duration_ms(42), "42ms");
    }
}
