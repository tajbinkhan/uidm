//! The automation API's JSON Schema (`docs/automation-api.schema.json`)
//! against the messages the code reads and writes: its examples must parse
//! as the wire messages, and the messages built in Rust must fit it, so the
//! schema and the wire format cannot drift apart.
//!
//! The schema uses a small part of JSON Schema, which `fits` checks:
//! `$ref` into `$defs`, `oneOf`, `anyOf`, `not`, `const`, `enum`, `type`,
//! `minimum`, `maximum`, `required`, `properties`, `additionalProperties`
//! and `items`.

// The whole file is a test: its helpers fail the test the way `#[test]`
// functions do, which clippy.toml's test exemptions do not see here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{json, Value};

use udm_application::daemon::AUTOMATION_API;
use udm_application::remote::Listing;
use udm_domain::address::Link;
use udm_domain::bytes::ByteCount;
use udm_domain::checksum::Checksum;
use udm_domain::download::{Answer, Download, DownloadEntry};
use udm_domain::id::{DownloadId, QueueId};
use udm_domain::queue::Queue;
use udm_domain::settings::Duplicate;
use udm_infrastructure::protocol::{Incoming, Outgoing};

const SCHEMA: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/automation-api.schema.json"));

fn schema() -> Value {
    serde_json::from_str(SCHEMA).expect("the schema is JSON")
}

/// The definition `name` in the schema's `$defs`.
fn def<'a>(root: &'a Value, name: &str) -> &'a Value {
    root["$defs"].get(name).unwrap_or_else(|| panic!("no $defs/{name}"))
}

/// Why `value` does not fit `schema`, if it does not.
fn fits(root: &Value, schema: &Value, value: &Value, at: &str) -> Result<(), String> {
    if let Some(r) = schema.get("$ref").and_then(Value::as_str) {
        let name = r.strip_prefix("#/$defs/").unwrap_or_else(|| panic!("unknown $ref {r}"));
        return fits(root, def(root, name), value, at);
    }
    if let Some(all) = schema.get("oneOf").and_then(Value::as_array) {
        let n = all.iter().filter(|s| fits(root, s, value, at).is_ok()).count();
        if n != 1 {
            return Err(format!("{at}: fits {n} of oneOf, not 1: {value}"));
        }
    }
    if let Some(any) = schema.get("anyOf").and_then(Value::as_array) {
        if !any.iter().any(|s| fits(root, s, value, at).is_ok()) {
            return Err(format!("{at}: fits none of anyOf: {value}"));
        }
    }
    if let Some(not) = schema.get("not") {
        if fits(root, not, value, at).is_ok() {
            return Err(format!("{at}: fits what it must not: {value}"));
        }
    }
    if let Some(c) = schema.get("const") {
        if c != value {
            return Err(format!("{at}: {value} is not {c}"));
        }
    }
    if let Some(e) = schema.get("enum").and_then(Value::as_array) {
        if !e.contains(value) {
            return Err(format!("{at}: {value} is not one of {e:?}"));
        }
    }
    if let Some(t) = schema.get("type") {
        let types: Vec<&str> = match t {
            Value::String(s) => vec![s.as_str()],
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            _ => panic!("bad type {t}"),
        };
        let is = |t: &str| match t {
            "integer" => value.is_i64() || value.is_u64(),
            "number" => value.is_number(),
            "string" => value.is_string(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            "object" => value.is_object(),
            "array" => value.is_array(),
            other => panic!("unknown type {other}"),
        };
        if !types.iter().any(|t| is(t)) {
            return Err(format!("{at}: {value} is not {types:?}"));
        }
    }
    if let Some(n) = value.as_f64() {
        if schema.get("minimum").and_then(Value::as_f64).is_some_and(|min| n < min) {
            return Err(format!("{at}: {n} is below the minimum"));
        }
        if schema.get("maximum").and_then(Value::as_f64).is_some_and(|max| n > max) {
            return Err(format!("{at}: {n} is above the maximum"));
        }
    }
    if let Some(object) = value.as_object() {
        for key in schema.get("required").and_then(Value::as_array).into_iter().flatten() {
            let key = key.as_str().unwrap_or_default();
            if !object.contains_key(key) {
                return Err(format!("{at}: {key} is missing from {value}"));
            }
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        for (key, v) in object {
            match properties.and_then(|p| p.get(key)) {
                Some(s) => fits(root, s, v, &format!("{at}.{key}"))?,
                None => {
                    if let Some(extra) = schema.get("additionalProperties").filter(|s| s.is_object()) {
                        fits(root, extra, v, &format!("{at}.{key}"))?;
                    }
                }
            }
        }
    }
    if let (Some(items), Some(array)) = (schema.get("items"), value.as_array()) {
        for (i, v) in array.iter().enumerate() {
            fits(root, items, v, &format!("{at}[{i}]"))?;
        }
    }
    Ok(())
}

fn assert_fits(root: &Value, name: &str, value: &Value) {
    if let Err(why) = fits(root, def(root, name), value, name) {
        panic!("{why}");
    }
}

/// The definitions `name`'s `oneOf` lists.
fn members<'a>(root: &'a Value, name: &str) -> Vec<&'a Value> {
    def(root, name)["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| def(root, r["$ref"].as_str().unwrap().strip_prefix("#/$defs/").unwrap()))
        .collect()
}

#[test]
fn the_schema_is_this_versions() {
    assert_eq!(schema()["x-udm-api"], json!(AUTOMATION_API));
}

#[test]
fn every_request_example_fits_and_the_daemon_reads_it() {
    let root = schema();
    for d in members(&root, "request") {
        let examples = d["examples"].as_array().unwrap_or_else(|| panic!("no examples in {d}"));
        assert!(!examples.is_empty());
        for example in examples {
            assert_fits(&root, "request", example);
            let read: Incoming = serde_json::from_value(example.clone()).unwrap_or_else(|e| panic!("{example}: {e}"));
            // What the daemon read, written back, fits as well.
            assert_fits(&root, "request", &serde_json::to_value(read).unwrap());
        }
    }
}

#[test]
fn every_event_example_fits_and_programs_read_it() {
    let root = schema();
    for d in members(&root, "event") {
        let examples = d["examples"].as_array().unwrap_or_else(|| panic!("no examples in {d}"));
        for example in examples {
            assert_fits(&root, "event", example);
            let read: Outgoing = serde_json::from_value(example.clone()).unwrap_or_else(|e| panic!("{example}: {e}"));
            assert_fits(&root, "event", &serde_json::to_value(read).unwrap());
        }
    }
}

fn entry() -> DownloadEntry {
    serde_json::from_value(json!({
        "id": 7, "url": "https://example.com/a.iso", "path": "/dl/a.iso", "total": 4, "done": 4,
        "status": "done", "error": null, "resumable": true, "connections": 2,
        "checksum": "crc32:d87f7e0c", "last_try": 1_700_000_000u64,
    }))
    .unwrap()
}

#[test]
fn the_requests_the_daemon_takes_fit() {
    let root = schema();
    let id = DownloadId::new(7);
    let add = Download {
        url: Link::new("https://example.com/a.iso"),
        folder: Some("/dl".into()),
        filename: Some("a.iso".into()),
        headers: [("X-Token".to_string(), "1".to_string())].into(),
        connections: Some(4),
        checksum: Some(Checksum::parse("crc32:d87f7e0c").unwrap()),
        start: Some(true),
        ask: Some(false),
        duplicate: Some(Duplicate::NewName),
        queue: Some(QueueId::new(1)),
        ..Download::default()
    };
    let requests = [
        Incoming::Hello { version: Some("1".into()), client: Some("udm-cli".into()) },
        Incoming::Ping,
        Incoming::Add(add),
        Incoming::List,
        Incoming::Cancel { id },
        Incoming::Retry { id },
        Incoming::Confirm(Answer::start(id)),
        Incoming::Remove { id, delete_files: true },
        Incoming::PauseAll,
        Incoming::ResumeAll,
        Incoming::StartQueue { id: QueueId::new(1) },
        Incoming::StopQueue { id: QueueId::new(2) },
    ];
    for request in requests {
        assert_fits(&root, "request", &serde_json::to_value(request).unwrap());
    }
    // A hello without a client is the browser extension's, and the window's
    // is not a program's.
    assert!(fits(&root, def(&root, "request"), &json!({ "op": "hello" }), "hello").is_err());
    assert!(fits(&root, def(&root, "request"), &json!({ "op": "hello", "client": "window" }), "hello").is_err());
}

#[test]
fn the_events_the_daemon_sends_fit() {
    let root = schema();
    let (id, url) = (DownloadId::new(7), Link::new("https://example.com/a.iso"));
    let events = [
        Outgoing::Hello { name: "udm-daemon".into(), version: "0.5.0".into(), api: Some(AUTOMATION_API) },
        Outgoing::Pong,
        Outgoing::Ack { id, url: url.clone(), status: "accepted".into() },
        Outgoing::Rejected { url: url.clone(), message: "no".into(), error_kind: Some(json!({ "kind": "other" })) },
        Outgoing::Error { message: "no".into(), error_kind: None },
        Outgoing::Added { id, url: url.clone() },
        Outgoing::Started {
            id,
            url: url.clone(),
            path: "/dl/a.iso".into(),
            total: None,
            resumable: false,
            connections: 1,
            partial: Some("/tmp/a.iso".into()),
            validator: None,
            country: udm_domain::country::CountryCode::parse("US"),
        },
        Outgoing::Progress {
            id,
            url: url.clone(),
            done: ByteCount::new(2),
            total: Some(ByteCount::new(4)),
            parts: None,
            segments: None,
        },
        Outgoing::Done { id, url: url.clone(), path: "/dl/a.iso".into(), note: Some("saved".into()) },
        Outgoing::Failed { id, url: url.clone(), error: "HTTP 404".into() },
        Outgoing::Canceled { id, url },
        Outgoing::Removed { id },
        Outgoing::Updated { item: entry() },
        Outgoing::Downloads { items: vec![entry()] },
        Outgoing::Queues { items: vec![Queue::main(), Queue::new(QueueId::new(2), "Night")] },
    ];
    for event in events {
        assert_fits(&root, "event", &serde_json::to_value(event).unwrap());
    }
}

#[test]
fn the_tools_list_fits() {
    let root = schema();
    let listing = Listing { downloads: vec![entry()], queues: vec![Queue::main()] };
    let printed: Value = serde_json::from_str(&udm_infrastructure::tool::json(&listing).unwrap()).unwrap();
    assert_fits(&root, "listing", &printed);
}
