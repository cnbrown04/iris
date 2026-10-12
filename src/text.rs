/// Small text helpers shared by the daemon, the list and the TUI.

/// Truncate `s` to `max` chars and mark the cut with an ellipsis.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    } else {
        s.to_string()
    }
}

/// A short relative age for an RFC 3339 timestamp: `now`, `12m`, `3h`, `2d`.
pub fn rel_age(ts: Option<&str>) -> String {
    let Some(s) = ts else {
        return String::new();
    };
    let Ok(created) = time::OffsetDateTime::parse(
        s,
        &time::format_description::well_known::Rfc3339,
    ) else {
        return String::new();
    };
    let now = time::OffsetDateTime::now_utc();
    let secs = (now - created).whole_seconds().max(0);
    match secs {
        0..=59 => "now".to_string(),
        60..=3_599 => format!("{}m", secs / 60),
        3_600..=86_399 => format!("{}h", secs / 3_600),
        _ => format!("{}d", secs / 86_400),
    }
}
