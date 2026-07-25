/// Minimal iCalendar (RFC 5545) VTODO parser — just enough to pull the
/// fields we need out of a calendar-data blob returned by CalDAV.
#[derive(Debug, Clone)]
pub struct Task {
    pub uid: String,
    pub summary: String,
    pub status: Option<String>,
    pub priority: Option<u32>,
    pub due: Option<String>,
    pub description: Option<String>,
    /// CalDAV resource href this task was parsed from (relative to the
    /// collection URL), used to PUT the task back when it's completed.
    pub href: String,
}

/// Undo RFC 5545 line folding (continuation lines start with a space or tab).
fn unfold(input: &str) -> String {
    let normalized = input.replace("\r\n", "\n");
    let mut result = String::new();
    for line in normalized.split('\n') {
        if (line.starts_with(' ') || line.starts_with('\t')) && !result.is_empty() {
            result.push_str(&line[1..]);
        } else {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(line);
        }
    }
    result
}

fn unescape_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(',') => out.push(','),
                Some(';') => out.push(';'),
                Some('\\') => out.push('\\'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Parse every VTODO block found in an unfolded (or raw) iCalendar text blob.
pub fn parse_vtodos(raw: &str) -> Vec<Task> {
    let text = unfold(raw);
    let mut tasks = Vec::new();

    let mut in_vtodo = false;
    let mut uid: Option<String> = None;
    let mut summary: Option<String> = None;
    let mut status: Option<String> = None;
    let mut priority: Option<u32> = None;
    let mut due: Option<String> = None;
    let mut description: Option<String> = None;

    for line in text.lines() {
        if line.eq_ignore_ascii_case("BEGIN:VTODO") {
            in_vtodo = true;
            uid = None;
            summary = None;
            status = None;
            priority = None;
            due = None;
            description = None;
            continue;
        }
        if line.eq_ignore_ascii_case("END:VTODO") {
            if in_vtodo {
                tasks.push(Task {
                    uid: uid.clone().unwrap_or_default(),
                    summary: summary.clone().unwrap_or_else(|| "(no summary)".to_string()),
                    status: status.clone(),
                    priority,
                    due: due.clone(),
                    description: description.clone(),
                    href: String::new(),
                });
            }
            in_vtodo = false;
            continue;
        }
        if !in_vtodo {
            continue;
        }

        let Some(idx) = line.find(':') else {
            continue;
        };
        let (name_part, value) = line.split_at(idx);
        let value = &value[1..];
        let name = name_part.split(';').next().unwrap_or(name_part).to_ascii_uppercase();

        match name.as_str() {
            "UID" => uid = Some(value.to_string()),
            "SUMMARY" => summary = Some(unescape_text(value)),
            "STATUS" => status = Some(value.trim().to_ascii_uppercase()),
            "PRIORITY" => priority = value.trim().parse().ok(),
            "DUE" => due = Some(value.trim().to_string()),
            "DESCRIPTION" => description = Some(unescape_text(value)),
            _ => {}
        }
    }

    tasks
}

/// A task counts as "open" unless it has been explicitly marked completed
/// or cancelled.
pub fn is_open(task: &Task) -> bool {
    !matches!(task.status.as_deref(), Some("COMPLETED") | Some("CANCELLED"))
}

/// Return `raw` with the VTODO whose UID matches `uid` marked completed
/// (STATUS/PERCENT-COMPLETE/COMPLETED set), leaving every other line —
/// including any other VTODOs in the same resource — untouched.
///
/// Errors if no VTODO with that UID is found, so callers don't silently
/// PUT back an unchanged resource.
pub fn set_completed(raw: &str, uid: &str) -> anyhow::Result<String> {
    let stamp = format_utc_now_ical();
    let text = unfold(raw);

    let mut out = String::new();
    let mut in_vtodo = false;
    let mut vtodo_lines: Vec<String> = Vec::new();
    let mut vtodo_uid: Option<String> = None;
    let mut found = false;

    for line in text.lines() {
        if line.eq_ignore_ascii_case("BEGIN:VTODO") {
            in_vtodo = true;
            vtodo_lines.clear();
            vtodo_uid = None;
            continue;
        }
        if line.eq_ignore_ascii_case("END:VTODO") {
            if vtodo_uid.as_deref() == Some(uid) {
                found = true;
                vtodo_lines.retain(|l| {
                    let name = l.split(':').next().unwrap_or("").split(';').next().unwrap_or("");
                    !matches!(
                        name.to_ascii_uppercase().as_str(),
                        "STATUS" | "COMPLETED" | "PERCENT-COMPLETE"
                    )
                });
                vtodo_lines.push("STATUS:COMPLETED".to_string());
                vtodo_lines.push(format!("COMPLETED:{stamp}"));
                vtodo_lines.push("PERCENT-COMPLETE:100".to_string());
            }
            out.push_str("BEGIN:VTODO\r\n");
            for l in &vtodo_lines {
                out.push_str(l);
                out.push_str("\r\n");
            }
            out.push_str("END:VTODO\r\n");
            in_vtodo = false;
            continue;
        }
        if in_vtodo {
            if line.split(':').next().unwrap_or("").split(';').next().unwrap_or("").eq_ignore_ascii_case("UID")
                && let Some(idx) = line.find(':')
            {
                vtodo_uid = Some(line[idx + 1..].trim().to_string());
            }
            vtodo_lines.push(line.to_string());
        } else {
            out.push_str(line);
            out.push_str("\r\n");
        }
    }

    if !found {
        anyhow::bail!("no VTODO with UID {uid} found in ical resource");
    }

    Ok(out)
}

/// Current UTC time formatted as an RFC 5545 `DATE-TIME` (`YYYYMMDDTHHMMSSZ`).
fn format_utc_now_ical() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = secs.div_euclid(86400);
    let secs_of_day = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (secs_of_day / 3600, (secs_of_day % 3600) / 60, secs_of_day % 60);
    format!("{y:04}{m:02}{d:02}T{hh:02}{mm:02}{ss:02}Z")
}

/// Days-since-epoch to (year, month, day), civil calendar, UTC.
/// Howard Hinnant's `civil_from_days` algorithm (public domain) — chosen to
/// avoid pulling in a datetime crate for one timestamp.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_completed_marks_matching_uid() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\nUID:abc-123\r\nSUMMARY:Test task\r\nSTATUS:NEEDS-ACTION\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
        let updated = set_completed(ics, "abc-123").unwrap();
        assert!(updated.contains("STATUS:COMPLETED"));
        assert!(updated.contains("PERCENT-COMPLETE:100"));
        assert!(updated.contains("COMPLETED:"));
        assert!(!updated.contains("STATUS:NEEDS-ACTION"));
        assert!(updated.contains("UID:abc-123"));
    }

    #[test]
    fn set_completed_errors_on_missing_uid() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\nUID:other\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
        assert!(set_completed(ics, "abc-123").is_err());
    }

    #[test]
    fn civil_from_days_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19928), (2024, 7, 24));
    }
}
