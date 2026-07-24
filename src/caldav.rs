use anyhow::{bail, Context};
use quick_xml::events::Event;
use quick_xml::reader::Reader;

use crate::config::CaldavConfig;
use crate::ical::{self, Task};

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

/// Fetch every open (not completed/cancelled) VTODO from the configured
/// CalDAV task-list collection.
pub fn fetch_open_tasks(cfg: &CaldavConfig) -> anyhow::Result<Vec<Task>> {
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
pub fn mark_completed(cfg: &CaldavConfig, task: &Task) -> anyhow::Result<()> {
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
