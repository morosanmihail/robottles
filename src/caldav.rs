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

    let blocks = extract_calendar_data(&body).context("parsing CalDAV multistatus response")?;

    let tasks = blocks
        .iter()
        .flat_map(|block| ical::parse_vtodos(block))
        .filter(ical::is_open)
        .collect();

    Ok(tasks)
}

/// Pull out the raw contents of every `<calendar-data>` element in a
/// CalDAV multistatus XML response, regardless of namespace prefix.
fn extract_calendar_data(xml: &str) -> anyhow::Result<Vec<String>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut blocks = Vec::new();
    let mut capturing = false;
    let mut current = String::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) if e.local_name().as_ref() == b"calendar-data" => {
                capturing = true;
                current.clear();
            }
            Event::Text(t) if capturing => {
                current.push_str(&t.unescape()?);
            }
            Event::CData(t) if capturing => {
                current.push_str(&String::from_utf8_lossy(t.as_ref()));
            }
            Event::End(e) if e.local_name().as_ref() == b"calendar-data" => {
                capturing = false;
                blocks.push(std::mem::take(&mut current));
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(blocks)
}
