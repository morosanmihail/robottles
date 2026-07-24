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
