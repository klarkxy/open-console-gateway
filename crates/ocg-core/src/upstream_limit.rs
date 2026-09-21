//! I/O-free upstream 429 reset/window parsers shared by Gateway and Dashboard.
//!
//! Dashboard account Ping and managed-key verification consume this leaf
//! directly so limit text does not require a `crate::gateway` edge. HTTP,
//! cooldown writes, selector policy, and response mapping stay in the callers.
//!
//! This module must not import state, dashboard, dashboard_v3, gateway,
//! reqwest, database, or provider adapters.

use crate::models::UsageWindowKind;
use chrono::Duration;

/// 识别 opencode-go 429 对应的额度窗口。未知限流返回 `None`，避免误清手动用量。
pub fn parse_usage_limit_window(text: &str) -> Option<UsageWindowKind> {
    let text = text.to_ascii_lowercase();
    if text.contains("5-hour usage limit") || text.contains("5 hour usage limit") {
        Some(UsageWindowKind::FiveHours)
    } else if text.contains("weekly usage limit") {
        Some(UsageWindowKind::Week)
    } else if text.contains("monthly usage limit") {
        Some(UsageWindowKind::Month)
    } else if text.contains("freeusagelimiterror")
        || text.contains("free usage exceeded")
        || text.contains("free usage limit")
        || text.contains("free couta")
        || text.contains("free quota")
    {
        Some(UsageWindowKind::Free)
    } else {
        None
    }
}

/// 解析 opencode-go 429 消息中的重置时长。
///
/// 已知格式：
/// - "Monthly usage limit reached. Resets in 13 days."
/// - "Weekly usage limit reached. Resets in 4 days."
/// - "5-hour usage limit reached. Resets in 13min."
///
/// 返回冷却时长；无法识别返回 `None`（调用方退默认值）。
pub fn parse_reset(text: &str) -> Option<Duration> {
    let idx = text.find("Resets in")?;
    let mut total = Duration::zero();
    let mut pending = None;
    let mut found = false;

    for token in text[idx + "Resets in".len()..].split_whitespace() {
        let digit_end = token
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(token.len());
        let (n, unit) = if digit_end > 0 {
            (token[..digit_end].parse().ok(), &token[digit_end..])
        } else {
            (pending.take(), token)
        };
        let Some(n) = n else { continue };
        let unit = unit.trim_matches(|c: char| !c.is_ascii_alphabetic());
        let duration = match unit.to_ascii_lowercase().as_str() {
            u if u.starts_with("min") => Duration::try_minutes(n)?,
            u if u.starts_with("hr") || u.starts_with("hour") || u == "h" => {
                Duration::try_hours(n)?
            }
            u if u.starts_with("day") => Duration::try_days(n)?,
            "" => {
                pending = Some(n);
                continue;
            }
            _ => continue,
        };
        total = total.checked_add(&duration)?;
        found = true;
    }

    found.then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_messages() {
        assert_eq!(
            parse_reset("Monthly usage limit reached. Resets in 13 days."),
            Some(Duration::days(13))
        );
        assert_eq!(
            parse_reset("Weekly usage limit reached. Resets in 4 days."),
            Some(Duration::days(4))
        );
        assert_eq!(
            parse_reset("5-hour usage limit reached. Resets in 13min."),
            Some(Duration::minutes(13))
        );
        assert_eq!(
            parse_reset(
                r#"{"type":"GoUsageLimitError","message":"Weekly usage limit reached. Resets in 21hr 10min."}"#
            ),
            Some(Duration::hours(21) + Duration::minutes(10))
        );
        assert_eq!(
            parse_reset("Resets in  1  day."),
            Some(Duration::days(1)),
            "extra-whitespace"
        );
    }

    #[test]
    fn returns_none_for_unknown() {
        assert_eq!(parse_reset(""), None);
        assert_eq!(parse_reset("rate limit exceeded"), None);
        assert_eq!(parse_reset("Resets in sometime"), None);
    }

    #[test]
    fn identifies_only_known_usage_windows() {
        assert_eq!(
            parse_usage_limit_window("5-hour usage limit reached. Resets in 13min."),
            Some(UsageWindowKind::FiveHours)
        );
        assert_eq!(
            parse_usage_limit_window(
                r#"{"type":"FreeUsageLimitError","message":"Free usage exceeded"}"#
            ),
            Some(UsageWindowKind::Free)
        );
        assert_eq!(
            parse_usage_limit_window(
                r#"{"type":"GoUsageLimitError","message":"Weekly usage limit reached. Resets in 21hr 10min."}"#
            ),
            Some(UsageWindowKind::Week)
        );
        assert_eq!(
            parse_usage_limit_window("Monthly usage limit reached. Resets in 13 days."),
            Some(UsageWindowKind::Month)
        );
        assert_eq!(parse_usage_limit_window("rate limit exceeded"), None);
    }
}
