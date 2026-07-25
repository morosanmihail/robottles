use anyhow::{bail, Context};
use quick_xml::events::Event;
use quick_xml::reader::Reader;

use crate::config::CaldavConfig;
use crate::ical::{self, Task};
use crate::task_source::TaskSource;

const REPORT_BODY: &str = r#"<?xml version="1.0" encoding="utf-8" ?>
<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop>
    <d:getetag/>
    <c:calendar-data/>
  </d:prop>
  <c:filter>
    <c:comp-filter name="VCALENDAR">
      <c:comp-filter name="VTODO"/>
    </c:comp-filter>
  </c:filter>
</c:calendar-query>"#;

/// A task source backed by a CalDAV task-list (VTODO) collection.
pub struct CaldavTaskSource {
    pub config: CaldavConfig,
}

impl CaldavTaskSource {
    pub fn new(config: CaldavConfig) -> Self {
        Self { config }
    }
}

impl TaskSource for CaldavTaskSource {
    /// Fetch every open task, and return the most important one: lower
    /// PRIORITY number wins (RFC 5545: 1 = highest, 9 = lowest, 0/absent =
    /// unspecified and sorts last among prioritized tasks), then earliest
    /// DUE date, then summary as a tiebreaker.
    fn get_next_task(&self) -> anyhow::Result<Option<Task>> {
        let mut tasks = fetch_open_tasks(&self.config)?;
        sort_by_priority(&mut tasks);
        Ok(if tasks.is_empty() {
            None
        } else {
            Some(tasks.remove(0))
        })
    }

    fn mark_completed(&self, task: &Task) -> anyhow::Result<()> {
        mark_completed(&self.config, task)
    }
}

/// Order tasks "most important first"; see [`CaldavTaskSource::get_next_task`].
fn sort_by_priority(tasks: &mut [Task]) {
    tasks.sort_by(|a, b| {
        let pa = a.priority.filter(|&p| p > 0).unwrap_or(u32::MAX);
        let pb = b.priority.filter(|&p| p > 0).unwrap_or(u32::MAX);
        pa.cmp(&pb)
            .then_with(|| {
                let da = a.due.as_deref().unwrap_or("9999999999999");
                let db = b.due.as_deref().unwrap_or("9999999999999");
                da.cmp(db)
            })
            .then_with(|| a.summary.cmp(&b.summary))
    });
}

/// Fetch every open (not completed/cancelled) VTODO from the configured
/// CalDAV task-list collection.
fn fetch_open_tasks(cfg: &CaldavConfig) -> anyhow::Result<Vec<Task>> {
    let blocks = report_calendar_data(cfg)?;

    let tasks = blocks
        .iter()
        .flat_map(|block| {
            ical::parse_vtodos(&block.ical).into_iter().map(|mut task| {
                task.href = block.href.clone();
                task
            })
        })
        .filter(ical::is_open)
        .collect();

    Ok(tasks)
}

/// Mark `task`'s VTODO as completed on the CalDAV server: fetch the current
/// resource, flip STATUS/COMPLETED/PERCENT-COMPLETE, and PUT it back.
fn mark_completed(cfg: &CaldavConfig, task: &Task) -> anyhow::Result<()> {
    let base = reqwest::Url::parse(&cfg.url).context("parsing configured CalDAV url")?;
    let resource_url = base
        .join(&task.href)
        .with_context(|| format!("resolving task href {} against CalDAV base url", task.href))?;

    let client = reqwest::blocking::Client::new();

    let get_response = client
        .get(resource_url.clone())
        .basic_auth(&cfg.username, Some(&cfg.token))
        .send()
        .with_context(|| format!("fetching current VTODO from {resource_url}"))?;
    let get_status = get_response.status();
    let etag = get_response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let ical_body = get_response.text().context("reading VTODO body")?;
    if !get_status.is_success() {
        bail!("fetching VTODO {resource_url} returned {get_status}: {ical_body}");
    }

    let updated = ical::set_completed(&ical_body, &task.uid)
        .with_context(|| format!("marking task {} completed in ical text", task.uid))?;

    let mut request = client
        .put(resource_url.clone())
        .basic_auth(&cfg.username, Some(&cfg.token))
        .header("Content-Type", "text/calendar; charset=utf-8");
    if let Some(etag) = &etag {
        request = request.header("If-Match", etag.as_str());
    }

    let put_response = request
        .body(updated)
        .send()
        .with_context(|| format!("PUTting completed VTODO to {resource_url}"))?;
    let put_status = put_response.status();
    if !put_status.is_success() {
        let text = put_response.text().unwrap_or_default();
        bail!("marking task {} complete failed: {put_status}: {text}", task.uid);
    }

    Ok(())
}

/// One `<d:response>` entry from a CalDAV multistatus reply: the resource
/// href and its raw `calendar-data` payload.
struct ResourceBlock {
    href: String,
    ical: String,
}

fn report_calendar_data(cfg: &CaldavConfig) -> anyhow::Result<Vec<ResourceBlock>> {
    let client = reqwest::blocking::Client::new();
    let method = reqwest::Method::from_bytes(b"REPORT").expect("valid method");

    let response = client
        .request(method, &cfg.url)
        .basic_auth(&cfg.username, Some(&cfg.token))
        .header("Depth", "1")
        .header("Content-Type", "application/xml; charset=utf-8")
        .body(REPORT_BODY)
        .send()
        .with_context(|| format!("sending CalDAV REPORT request to {}", cfg.url))?;

    let status = response.status();
    let body = response.text().context("reading CalDAV response body")?;
    if !status.is_success() {
        bail!("CalDAV server returned {status}: {body}");
    }

    extract_calendar_data(&body).context("parsing CalDAV multistatus response")
}

/// Pull out every `<d:response>`'s href and `<calendar-data>` contents from
/// a CalDAV multistatus XML response, regardless of namespace prefix.
fn extract_calendar_data(xml: &str) -> anyhow::Result<Vec<ResourceBlock>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut blocks = Vec::new();
    let mut in_href = false;
    let mut in_calendar_data = false;
    let mut current_href = String::new();
    let mut current_ical = String::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => match e.local_name().as_ref() {
                b"response" => {
                    current_href.clear();
                    current_ical.clear();
                }
                b"href" => in_href = true,
                b"calendar-data" => {
                    in_calendar_data = true;
                    current_ical.clear();
                }
                _ => {}
            },
            Event::Text(t) if in_href => {
                current_href.push_str(&t.unescape()?);
            }
            Event::Text(t) if in_calendar_data => {
                current_ical.push_str(&t.unescape()?);
            }
            Event::CData(t) if in_calendar_data => {
                current_ical.push_str(&String::from_utf8_lossy(t.as_ref()));
            }
            Event::End(e) => match e.local_name().as_ref() {
                b"href" => in_href = false,
                b"calendar-data" => in_calendar_data = false,
                b"response" => {
                    if !current_ical.is_empty() {
                        blocks.push(ResourceBlock {
                            href: std::mem::take(&mut current_href),
                            ical: std::mem::take(&mut current_ical),
                        });
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_with(uid: &str, summary: &str, priority: Option<u32>, due: Option<&str>) -> Task {
        Task {
            uid: uid.to_string(),
            summary: summary.to_string(),
            status: None,
            priority,
            due: due.map(str::to_string),
            description: None,
            href: String::new(),
        }
    }

    #[test]
    fn sort_by_priority_orders_by_priority_then_due_then_summary() {
        let mut tasks = vec![
            task_with("a", "Zebra", Some(5), None),
            task_with("b", "Apple", None, None),
            task_with("c", "Urgent", Some(1), Some("20260801T000000Z")),
            task_with("d", "Also urgent", Some(1), Some("20260701T000000Z")),
        ];
        sort_by_priority(&mut tasks);
        let order: Vec<&str> = tasks.iter().map(|t| t.uid.as_str()).collect();
        assert_eq!(order, vec!["d", "c", "a", "b"]);
    }

    #[test]
    fn sort_by_priority_treats_zero_priority_as_unspecified() {
        let mut tasks = vec![
            task_with("a", "Has priority", Some(3), None),
            task_with("b", "Zero priority", Some(0), None),
        ];
        sort_by_priority(&mut tasks);
        let order: Vec<&str> = tasks.iter().map(|t| t.uid.as_str()).collect();
        assert_eq!(order, vec!["a", "b"]);
    }
}
