//! `udm-mcp`'s side of the Model Context Protocol: JSON-RPC 2.0 over
//! standard input and output, one message a line, as AI assistants (Claude
//! Desktop, Claude Code, Cursor, VS Code) start a local MCP server.
//!
//! It speaks the part of the protocol a server of tools needs: `initialize`,
//! `ping`, `tools/list` and `tools/call`, and takes the client's
//! notifications (`notifications/initialized`, `notifications/cancelled`)
//! without answering them. Each tool's arguments become a
//! `udm_application::remote::Request` for the `Tools` port, and its
//! `Reply` becomes the tool's result, as text and as structured content.
//!
//! Standard output carries the protocol alone; the program logs to its file.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use udm_application::remote::{AddRequest, RemoteError, Reply, Request, Resumed, Tools, Which};
use udm_domain::address::Address;
use udm_domain::download::{DownloadEntry, Status};
use udm_domain::filename::FileName;
use udm_domain::id::DownloadId;
use udm_domain::queue::Queue;

/// The protocol versions this server speaks, the newest first. A client
/// asking for one of them gets it; any other gets the newest.
pub const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// What the server calls itself.
const NAME: &str = "udm-mcp";

/// The first version with tool annotations, and the first with tool titles
/// and structured results.
const ANNOTATIONS_SINCE: &str = "2025-03-26";
const STRUCTURED_SINCE: &str = "2025-06-18";

// JSON-RPC's error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// What the assistant is told about the server when it connects.
const INSTRUCTIONS: &str = "UDM is the user's download manager. These tools list its downloads and queues, \
add links (to start now or to wait in a queue), pause and resume downloads, take them out of the list and start \
or stop queues. Downloads go on in UDM after a tool returns; call list_downloads to see how they are doing. \
Download ids come from list_downloads or add_download. Never delete a downloaded file unless the user has \
confirmed it in this conversation.";

/// Answer the client's messages from `input` on `output` until `input`
/// ends, handing each tool call to `tools`. `version` is the program's.
pub fn serve(input: impl BufRead, mut output: impl Write, tools: &mut impl Tools, version: &str) -> io::Result<()> {
    let mut server = Server::new(tools, version);
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(answer) = server.handle(&line) {
            // A Value always serializes; the line ends the message.
            let text = serde_json::to_string(&answer).map_err(io::Error::other)?;
            output.write_all(text.as_bytes())?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
    Ok(())
}

/// One client's conversation with the server.
pub struct Server<'t, T: Tools> {
    tools: &'t mut T,
    version: String,
    /// The protocol version agreed in `initialize`; the newest until then.
    protocol: &'static str,
}

impl<'t, T: Tools> Server<'t, T> {
    pub fn new(tools: &'t mut T, version: &str) -> Self {
        Server { tools, version: version.to_string(), protocol: PROTOCOL_VERSIONS[0] }
    }

    /// The answer to one line from the client, if it needs one.
    pub fn handle(&mut self, line: &str) -> Option<Value> {
        match serde_json::from_str::<Value>(line) {
            Err(e) => Some(error(Value::Null, PARSE_ERROR, &format!("not JSON: {e}"))),
            Ok(Value::Array(batch)) if batch.is_empty() => Some(error(Value::Null, INVALID_REQUEST, "an empty batch")),
            Ok(Value::Array(batch)) => {
                let answers: Vec<Value> = batch.into_iter().filter_map(|m| self.message(m)).collect();
                (!answers.is_empty()).then_some(Value::Array(answers))
            }
            Ok(message) => self.message(message),
        }
    }

    /// The answer to one message: none for a notification, or for a reply
    /// to a request (the server sends none).
    fn message(&mut self, message: Value) -> Option<Value> {
        let Value::Object(message) = message else {
            return Some(error(Value::Null, INVALID_REQUEST, "a message is a JSON object"));
        };
        let id = message.get("id").cloned();
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            if message.contains_key("result") || message.contains_key("error") {
                return None;
            }
            return Some(error(id.unwrap_or(Value::Null), INVALID_REQUEST, "no method"));
        };
        let params = message.get("params").cloned().unwrap_or(Value::Object(Map::new()));
        let Some(id) = id else {
            // A notification: initialized, cancelled (a call here is never
            // in progress while the next message is read), or one this
            // server does not know.
            tracing::debug!(method, "notification");
            return None;
        };
        Some(match self.call(method, &params) {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, why)) => error(id, code, &why),
        })
    }

    fn call(&mut self, method: &str, params: &Value) -> Result<Value, (i64, String)> {
        match method {
            "initialize" => Ok(self.initialize(params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tool_list(self.protocol) })),
            "tools/call" => self.tool_call(params),
            _ => Err((METHOD_NOT_FOUND, format!("no method {method}"))),
        }
    }

    fn initialize(&mut self, params: &Value) -> Value {
        let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or_default();
        self.protocol = PROTOCOL_VERSIONS.iter().find(|v| **v == asked).copied().unwrap_or(PROTOCOL_VERSIONS[0]);
        let client = params.pointer("/clientInfo/name").and_then(Value::as_str).unwrap_or("an unnamed client");
        tracing::info!(client, asked, agreed = self.protocol, "initialize");
        json!({
            "protocolVersion": self.protocol,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": NAME, "title": "UDM", "version": self.version },
            "instructions": INSTRUCTIONS,
        })
    }

    fn tool_call(&mut self, params: &Value) -> Result<Value, (i64, String)> {
        let name = params.get("name").and_then(Value::as_str).ok_or((INVALID_PARAMS, "no tool name".to_string()))?;
        let empty = Value::Object(Map::new());
        let arguments = match params.get("arguments") {
            None | Some(Value::Null) => &empty,
            Some(a) if a.is_object() => a,
            Some(_) => return Err((INVALID_PARAMS, "the arguments must be a JSON object".to_string())),
        };
        if !TOOLS.iter().any(|t| t.name == name) {
            return Err((INVALID_PARAMS, format!("no tool {name}")));
        }
        // A wrong argument is the tool's error, so the assistant can see it
        // and try again.
        let request = match request(name, arguments) {
            Ok(request) => request,
            Err(why) => return Ok(failed(&why)),
        };
        tracing::info!(tool = name, "tool call");
        let filter = arguments.get("status").and_then(Value::as_str).map(str::to_string);
        Ok(match self.tools.run(request) {
            Ok(reply) => self.result(reply_json(reply, filter.as_deref())),
            Err(e) => {
                tracing::warn!(tool = name, error = %e, "refused");
                failed(&refusal(&e))
            }
        })
    }

    /// A tool's result: its JSON as text, and as structured content where
    /// the protocol has it.
    fn result(&self, value: Value) -> Value {
        let text = serde_json::to_string_pretty(&value).unwrap_or_default();
        let mut result = json!({ "content": [{ "type": "text", "text": text }], "isError": false });
        if self.protocol >= STRUCTURED_SINCE {
            result["structuredContent"] = value;
        }
        result
    }
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// A tool that failed, and why, for the assistant to read.
fn failed(why: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": why }], "isError": true })
}

/// Why the daemon did not do it, with what to try for the common cases.
fn refusal(e: &RemoteError) -> String {
    match e {
        RemoteError::NoDownload(_) => format!("{e}. Call list_downloads for the ids."),
        RemoteError::NoQueue(_) => format!("{e}. Call list_downloads for the queues."),
        RemoteError::Connect(_) => format!("{e}. UDM may not be installed next to udm-mcp."),
        _ => e.to_string(),
    }
}

/// A tool as `tools/list` describes it.
struct Tool {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    /// Its arguments, as a JSON Schema object's `properties` and `required`.
    schema: fn() -> Value,
    read_only: bool,
    destructive: bool,
    idempotent: bool,
}

const TOOLS: [Tool; 7] = [
    Tool {
        name: "list_downloads",
        title: "List downloads",
        description: "List UDM's downloads (id, file name, status, bytes done and in all, address, saved path, \
error) and its queues (id, name, whether started, the download ids in it). Optionally only the downloads with one \
status.",
        schema: list_schema,
        read_only: true,
        destructive: false,
        idempotent: true,
    },
    Tool {
        name: "add_download",
        title: "Add a download",
        description: "Add a link (an http, https, ftp or ftps address, or a torrent's magnet link) to UDM. Without a \
queue it starts now; \
with a queue it waits there until the queue starts. No dialog is shown. If the address is in the list already, it \
is downloaded again under a numbered name. Returns the new download's id; the download goes on in UDM, so \
call list_downloads to follow it.",
        schema: add_schema,
        read_only: false,
        destructive: false,
        idempotent: false,
    },
    Tool {
        name: "pause_download",
        title: "Pause a download",
        description: "Stop a running download, keeping what it has downloaded so it can resume later. Give its id, \
or all: true to stop every running download and started queue.",
        schema: which_schema,
        read_only: false,
        destructive: false,
        idempotent: true,
    },
    Tool {
        name: "resume_download",
        title: "Resume a download",
        description: "Start a stopped, failed or waiting download again from where it stopped. Give its id, or \
all: true to resume what pause_download with all stopped. A stopped download goes on as a new download with a new \
id, which the result gives.",
        schema: which_schema,
        read_only: false,
        destructive: false,
        idempotent: true,
    },
    Tool {
        name: "remove_download",
        title: "Remove a download",
        description: "Take a download out of UDM's list. A running download cannot be removed; pause it first. \
Its file stays on disk unless delete_files is true, which moves the downloaded file to the Recycle Bin or trash. \
Never set delete_files to true unless the user has explicitly confirmed, in this conversation, that the file \
itself should be deleted: ask them first, naming the file.",
        schema: remove_schema,
        read_only: false,
        destructive: true,
        idempotent: false,
    },
    Tool {
        name: "start_queue",
        title: "Start a queue",
        description: "Start one of UDM's queues, so its waiting downloads run. Give the queue's name or id; \
without one, the main queue starts.",
        schema: queue_schema,
        read_only: false,
        destructive: false,
        idempotent: true,
    },
    Tool {
        name: "stop_queue",
        title: "Stop a queue",
        description: "Stop one of UDM's queues, pausing the downloads it runs. Give the queue's name or id; \
without one, the main queue stops.",
        schema: queue_schema,
        read_only: false,
        destructive: false,
        idempotent: true,
    },
];

/// The statuses a download can have, as the list spells them.
const STATUSES: [Status; 8] = [
    Status::Queued,
    Status::Downloading,
    Status::Done,
    Status::Failed,
    Status::Canceled,
    Status::Interrupted,
    Status::Confirming,
    Status::Waiting,
];

fn list_schema() -> Value {
    let statuses: Vec<String> = STATUSES.iter().map(|s| status(*s)).collect();
    json!({
        "properties": {
            "status": {
                "type": "string",
                "enum": statuses,
                "description": "Only downloads with this status. queued and downloading are running; canceled is \
    paused; waiting is in a queue; confirming waits for the save-as dialog; interrupted was running when UDM \
    stopped.",
            },
        },
    })
}

fn add_schema() -> Value {
    json!({
        "properties": {
            "url": { "type": "string", "description": "The address to download: http, https, ftp or ftps, or a magnet link." },
            "folder": {
                "type": "string",
                "description": "The folder to save in, a full path. Without it, UDM's settings pick the \
    folder by the file's type.",
            },
            "filename": { "type": "string", "description": "The name to save the file as. Without it, the \
    server's name." },
            "queue": {
                "type": ["string", "integer"],
                "description": "A queue's name or id: the download waits there instead of starting now.",
            },
        },
        "required": ["url"],
    })
}

fn which_schema() -> Value {
    json!({
        "properties": {
            "id": { "type": "integer", "minimum": 1, "description": "The download's id, from list_downloads." },
            "all": { "type": "boolean", "description": "true for every download instead of one id." },
        },
    })
}

fn remove_schema() -> Value {
    json!({
        "properties": {
            "id": { "type": "integer", "minimum": 1, "description": "The download's id, from list_downloads." },
            "delete_files": {
                "type": "boolean",
                "default": false,
                "description": "Also move the downloaded file to the Recycle Bin or trash. Only after the user \
    has confirmed it in this conversation.",
            },
        },
        "required": ["id"],
    })
}

fn queue_schema() -> Value {
    json!({
        "properties": {
            "queue": { "type": ["string", "integer"], "description": "The queue's name or id; the main queue \
    without it." },
        },
    })
}

/// `tools/list`'s tools, with what `protocol` has of titles and hints.
fn tool_list(protocol: &str) -> Vec<Value> {
    TOOLS
        .iter()
        .map(|t| {
            let mut schema = json!({ "type": "object", "additionalProperties": false });
            if let Value::Object(parts) = (t.schema)() {
                for (key, value) in parts {
                    schema[key] = value;
                }
            }
            let mut tool = json!({ "name": t.name, "description": t.description, "inputSchema": schema });
            if protocol >= STRUCTURED_SINCE {
                tool["title"] = json!(t.title);
            }
            if protocol >= ANNOTATIONS_SINCE {
                tool["annotations"] = json!({
                    "title": t.title,
                    "readOnlyHint": t.read_only,
                    "destructiveHint": t.destructive,
                    "idempotentHint": t.idempotent,
                    "openWorldHint": t.name == "add_download",
                });
            }
            tool
        })
        .collect()
}

/// The request tool `name` makes of `args`, or why they are wrong.
fn request(name: &str, args: &Value) -> Result<Request, String> {
    Ok(match name {
        "list_downloads" => {
            if let Some(s) = args.get("status").filter(|s| !s.is_null()) {
                let known = s.as_str().is_some_and(|s| STATUSES.iter().any(|k| status(*k) == s));
                if !known {
                    return Err(format!("status is one of {}", STATUSES.map(status).join(", ")));
                }
            }
            Request::List
        }
        "add_download" => Request::Add(add_request(args)?),
        "pause_download" => Request::Pause(which(args)?),
        "resume_download" => Request::Resume(which(args)?),
        "remove_download" => {
            let delete_files = match args.get("delete_files") {
                None | Some(Value::Null) => false,
                Some(Value::Bool(b)) => *b,
                Some(_) => return Err("delete_files is true or false".into()),
            };
            Request::Remove { id: download_id(args)?.ok_or("give the download's id")?, delete_files }
        }
        "start_queue" | "stop_queue" => Request::Queue { name: queue_name(args)?, start: name == "start_queue" },
        _ => return Err(format!("no tool {name}")),
    })
}

fn add_request(args: &Value) -> Result<AddRequest, String> {
    let url = text(args, "url")?.ok_or("give the url to download")?;
    let url = url.trim();
    let url = Address::parse(url)
        .map_err(|_| format!("{url} is not an http, https, ftp or ftps address or a magnet link"))?;
    let folder = match text(args, "folder")?.map(str::trim).filter(|f| !f.is_empty()) {
        Some(f) if Path::new(f).is_absolute() => Some(PathBuf::from(f).display().to_string()),
        Some(f) => return Err(format!("the folder {f} is not a full path")),
        None => None,
    };
    let filename = match text(args, "filename")?.map(str::trim).filter(|f| !f.is_empty()) {
        Some(f) => Some(FileName::parse(f).map_err(|e| format!("the file name {f}: {e}"))?),
        None => None,
    };
    Ok(AddRequest { url, folder, filename, queue: queue_name(args)? })
}

/// One download or all, from `id` or `all`.
fn which(args: &Value) -> Result<Which, String> {
    let all = match args.get("all") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("all is true or false".into()),
    };
    match (download_id(args)?, all) {
        (Some(_), true) => Err("give an id or all: true, not both".into()),
        (Some(id), false) => Ok(Which::One(id)),
        (None, true) => Ok(Which::All),
        (None, false) => Err("give the download's id, or all: true".into()),
    }
}

/// The download `id` names, a number or a number as text.
fn download_id(args: &Value) -> Result<Option<DownloadId>, String> {
    let n = match args.get("id") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::String(s)) => s.trim().parse::<u64>().ok(),
        Some(_) => None,
    };
    match n.filter(|n| *n > 0) {
        Some(n) => Ok(Some(DownloadId::new(n))),
        None => Err("id is a download's number, from list_downloads".into()),
    }
}

/// The queue `queue` names, by name or id.
fn queue_name(args: &Value) -> Result<Option<String>, String> {
    match args.get("queue") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.trim().to_string())),
        Some(Value::Number(n)) if n.is_u64() => Ok(Some(n.to_string())),
        Some(_) => Err("queue is a queue's name or id".into()),
    }
}

fn text<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(format!("{key} is text")),
    }
}

/// A status as the list spells it.
fn status(s: Status) -> String {
    serde_json::to_value(s).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

/// What a reply tells the assistant. `only` keeps the downloads with that
/// status in a list.
fn reply_json(reply: Reply, only: Option<&str>) -> Value {
    match reply {
        Reply::Listing(listing) => json!({
            "downloads": listing
                .downloads
                .iter()
                .filter(|e| only.is_none_or(|s| status(e.status) == s))
                .map(entry_json)
                .collect::<Vec<_>>(),
            "queues": listing.queues.iter().map(queue_json).collect::<Vec<_>>(),
        }),
        Reply::Added(added) => match added.queue {
            Some(q) => json!({
                "id": added.id,
                "queue": { "id": q.id, "name": q.name },
                "message": format!("Added as download {}, waiting in the queue {}.", added.id, q.name),
            }),
            None => json!({
                "id": added.id,
                "queue": null,
                "message": format!("Added as download {}; it starts now.", added.id),
            }),
        },
        Reply::Paused(Which::One(id)) => json!({ "id": id, "message": format!("Paused download {id}.") }),
        Reply::Paused(Which::All) => {
            json!({ "id": null, "message": "Paused every running download and started queue." })
        }
        Reply::Resumed(Some(Resumed::Running(id))) => {
            json!({ "id": id, "message": format!("Download {id} is running already.") })
        }
        Reply::Resumed(Some(Resumed::Started(id))) => {
            json!({ "id": id, "message": format!("Download {id} starts now.") })
        }
        Reply::Resumed(Some(Resumed::Retried { old, new })) => json!({
            "id": new,
            "previous_id": old,
            "message": format!("Download {old} goes on from its partial file as download {new}."),
        }),
        Reply::Resumed(None) => json!({ "id": null, "message": "Resumed what was paused." }),
        Reply::Removed { id, delete_files } => json!({
            "id": id,
            "files_deleted": delete_files,
            "message": if delete_files {
                format!("Removed download {id} and moved its file to the Recycle Bin or trash.")
            } else {
                format!("Removed download {id} from the list; its file stays on disk.")
            },
        }),
        Reply::Queue { queue, started } => json!({
            "queue": { "id": queue.id, "name": queue.name },
            "started": started,
            "message": format!("{} the queue {}.", if started { "Started" } else { "Stopped" }, queue.name),
        }),
    }
}

fn entry_json(e: &DownloadEntry) -> Value {
    let name = e
        .path
        .as_deref()
        .and_then(|p| Path::new(p).file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| e.url.to_string());
    json!({
        "id": e.id,
        "name": name,
        "status": status(e.status),
        "done_bytes": e.done,
        "total_bytes": e.total,
        "url": e.url,
        "path": e.path,
        "error": e.error,
    })
}

fn queue_json(q: &Queue) -> Value {
    json!({ "id": q.id, "name": q.name, "started": q.running, "downloads": q.order })
}

#[cfg(test)]
mod tests {
    use super::*;
    use udm_application::daemon::Failure;
    use udm_application::remote::{Added, Listing};
    use udm_domain::id::QueueId;

    /// Tools that record each request and answer it with `answer`.
    struct Recorder {
        asked: Vec<Request>,
        answer: fn(&Request) -> Result<Reply, RemoteError>,
    }

    impl Tools for Recorder {
        fn run(&mut self, request: Request) -> Result<Reply, RemoteError> {
            let answer = (self.answer)(&request);
            self.asked.push(request);
            answer
        }
    }

    fn row(id: u64, status: &str, path: Option<&str>) -> DownloadEntry {
        serde_json::from_value(json!({
            "id": id, "url": format!("https://dl.example/{id}.zip"), "status": status, "path": path,
            "connections": 0, "done": 5, "total": 10, "resumable": true,
        }))
        .unwrap()
    }

    fn answers(request: &Request) -> Result<Reply, RemoteError> {
        Ok(match request {
            Request::List => Reply::Listing(Listing {
                downloads: vec![row(1, "downloading", Some("/dl/a.iso")), row(2, "done", None)],
                queues: vec![Queue::main()],
            }),
            Request::Add(a) if a.queue.is_some() => {
                Reply::Added(Added { id: DownloadId::new(3), queue: Some(Queue::new(QueueId::new(2), "Night")) })
            }
            Request::Add(_) => Reply::Added(Added { id: DownloadId::new(3), queue: None }),
            Request::Pause(w) => Reply::Paused(*w),
            Request::Resume(Which::One(id)) => Reply::Resumed(Some(Resumed::Running(*id))),
            Request::Resume(Which::All) => Reply::Resumed(None),
            Request::Remove { id, .. } if id.get() == 1 => {
                return Err(RemoteError::Refused(Failure::Other("download 1 is running".into())))
            }
            Request::Remove { id, delete_files } => Reply::Removed { id: *id, delete_files: *delete_files },
            Request::Queue { name: Some(n), .. } if n == "Day" => return Err(RemoteError::NoQueue("Day".into())),
            Request::Queue { start, .. } => Reply::Queue { queue: Queue::main(), started: *start },
        })
    }

    fn recorder() -> Recorder {
        Recorder { asked: Vec::new(), answer: answers }
    }

    /// Feed `lines` through the server and read back each line it wrote.
    fn talk(tools: &mut Recorder, lines: &[Value]) -> Vec<Value> {
        let input: String = lines.iter().map(|l| format!("{l}\n")).collect();
        let mut output = Vec::new();
        serve(input.as_bytes(), &mut output, tools, "1.2.3").unwrap();
        String::from_utf8(output).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    fn initialize(version: &str) -> Value {
        json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
            "protocolVersion": version, "capabilities": {}, "clientInfo": { "name": "test", "version": "1" },
        }})
    }

    fn call(id: u64, name: &str, arguments: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": name, "arguments": arguments } })
    }

    /// The structured result of one tool call, after a 2025-06-18 handshake.
    fn result_of(tools: &mut Recorder, name: &str, arguments: Value) -> Value {
        let answers = talk(tools, &[initialize("2025-06-18"), call(1, name, arguments)]);
        answers[1]["result"].clone()
    }

    #[test]
    fn the_handshake_agrees_a_version_and_offers_tools() {
        let mut tools = recorder();
        let notified = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        let list = json!({ "jsonrpc": "2.0", "id": "a", "method": "tools/list" });
        let answers = talk(&mut tools, &[initialize("2025-06-18"), notified, list]);
        // The notification gets no answer.
        assert_eq!(answers.len(), 2);
        let init = &answers[0];
        assert_eq!(init["id"], 0);
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(init["result"]["serverInfo"]["name"], "udm-mcp");
        assert_eq!(init["result"]["serverInfo"]["version"], "1.2.3");
        assert!(init["result"]["capabilities"]["tools"].is_object());

        assert_eq!(answers[1]["id"], "a");
        let listed = answers[1]["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = listed.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "list_downloads",
                "add_download",
                "pause_download",
                "resume_download",
                "remove_download",
                "start_queue",
                "stop_queue"
            ]
        );
        for tool in listed {
            assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
            assert!(tool["title"].is_string());
            assert!(tool["annotations"]["readOnlyHint"].is_boolean());
        }
        assert_eq!(listed[1]["inputSchema"]["required"], json!(["url"]));
        assert!(tools.asked.is_empty());
    }

    #[test]
    fn an_older_client_gets_its_version_and_no_newer_fields() {
        let mut tools = recorder();
        let list = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
        let answers = talk(&mut tools, &[initialize("2024-11-05"), list, call(2, "list_downloads", json!({}))]);
        assert_eq!(answers[0]["result"]["protocolVersion"], "2024-11-05");
        let first = &answers[1]["result"]["tools"][0];
        assert!(first.get("annotations").is_none() && first.get("title").is_none());
        assert!(answers[2]["result"].get("structuredContent").is_none());
        assert!(answers[2]["result"]["content"][0]["text"].as_str().unwrap().contains("a.iso"));
    }

    #[test]
    fn an_unknown_version_gets_the_newest() {
        let answers = talk(&mut recorder(), &[initialize("1999-01-01")]);
        assert_eq!(answers[0]["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
    }

    #[test]
    fn the_list_is_compact_and_can_keep_one_status() {
        let mut tools = recorder();
        let result = result_of(&mut tools, "list_downloads", json!({}));
        assert_eq!(result["isError"], false);
        let list = &result["structuredContent"];
        assert_eq!(list["downloads"][0]["name"], "a.iso");
        assert_eq!(list["downloads"][0]["status"], "downloading");
        assert_eq!(
            (list["downloads"][0]["done_bytes"].clone(), list["downloads"][0]["total_bytes"].clone()),
            (json!(5), json!(10))
        );
        // Before it has a file, a download is named by its address.
        assert_eq!(list["downloads"][1]["name"], "https://dl.example/2.zip");
        assert_eq!(list["queues"][0]["name"], Queue::main().name);
        // The text is the same JSON, for clients that read only text.
        let text: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(&text, list);

        let done = result_of(&mut tools, "list_downloads", json!({ "status": "done" }));
        assert_eq!(done["structuredContent"]["downloads"].as_array().unwrap().len(), 1);
        let wrong = result_of(&mut tools, "list_downloads", json!({ "status": "sleeping" }));
        assert_eq!(wrong["isError"], true);
    }

    #[test]
    fn a_link_is_added_to_start_now_or_to_wait_in_a_queue() {
        let mut tools = recorder();
        let folder = if cfg!(windows) { r"C:\dl" } else { "/dl" };
        let now = result_of(
            &mut tools,
            "add_download",
            json!({ "url": " https://dl.example/a.iso ", "folder": folder, "filename": "b.iso" }),
        );
        assert_eq!(now["structuredContent"]["id"], 3);
        let later = result_of(&mut tools, "add_download", json!({ "url": "ftps://dl.example/a.iso", "queue": 2 }));
        assert_eq!(later["structuredContent"]["queue"]["name"], "Night");
        let [Request::Add(first), Request::Add(second)] = tools.asked.as_slice() else {
            panic!("{:?}", tools.asked);
        };
        assert_eq!(first.url.as_str(), "https://dl.example/a.iso");
        assert_eq!(first.folder.as_deref(), Some(folder));
        assert_eq!(first.filename.as_ref().map(FileName::as_str), Some("b.iso"));
        assert_eq!(first.queue, None);
        assert_eq!(second.queue.as_deref(), Some("2"));
    }

    #[test]
    fn wrong_arguments_are_the_tools_error_and_ask_nothing() {
        let mut tools = recorder();
        for (name, args) in [
            ("add_download", json!({})),
            ("add_download", json!({ "url": "file:///etc/passwd" })),
            ("add_download", json!({ "url": "https://x.example/a", "folder": "relative/dir" })),
            ("add_download", json!({ "url": "https://x.example/a", "filename": "../a" })),
            ("pause_download", json!({})),
            ("pause_download", json!({ "id": 1, "all": true })),
            ("resume_download", json!({ "id": 0 })),
            ("remove_download", json!({})),
            ("remove_download", json!({ "id": 2, "delete_files": "yes" })),
            ("start_queue", json!({ "queue": [1] })),
        ] {
            let result = result_of(&mut tools, name, args.clone());
            assert_eq!(result["isError"], true, "{name} {args}");
            assert!(result["content"][0]["text"].as_str().is_some_and(|t| !t.is_empty()));
        }
        assert!(tools.asked.is_empty(), "{:?}", tools.asked);
    }

    #[test]
    fn removing_keeps_the_files_unless_deleting_them_is_asked_for() {
        let mut tools = recorder();
        let kept = result_of(&mut tools, "remove_download", json!({ "id": 2 }));
        assert_eq!(kept["structuredContent"]["files_deleted"], false);
        let deleted = result_of(&mut tools, "remove_download", json!({ "id": "2", "delete_files": true }));
        assert_eq!(deleted["structuredContent"]["files_deleted"], true);
        assert_eq!(
            tools.asked,
            [
                Request::Remove { id: DownloadId::new(2), delete_files: false },
                Request::Remove { id: DownloadId::new(2), delete_files: true }
            ]
        );
        let list = talk(&mut recorder(), &[json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })]);
        let remove = &list[0]["result"]["tools"][4];
        assert_eq!(remove["annotations"]["destructiveHint"], true);
        assert_eq!(remove["inputSchema"]["properties"]["delete_files"]["default"], false);
        assert!(remove["description"].as_str().unwrap().contains("explicitly confirmed"));
    }

    #[test]
    fn pause_resume_and_queues_go_through() {
        let mut tools = recorder();
        result_of(&mut tools, "pause_download", json!({ "id": 4 }));
        result_of(&mut tools, "pause_download", json!({ "all": true }));
        let resumed = result_of(&mut tools, "resume_download", json!({ "id": 4 }));
        assert_eq!(resumed["structuredContent"]["id"], 4);
        result_of(&mut tools, "resume_download", json!({ "all": true }));
        let started = result_of(&mut tools, "start_queue", json!({}));
        assert_eq!(started["structuredContent"]["started"], true);
        result_of(&mut tools, "stop_queue", json!({ "queue": "Night" }));
        let id = DownloadId::new(4);
        assert_eq!(
            tools.asked,
            [
                Request::Pause(Which::One(id)),
                Request::Pause(Which::All),
                Request::Resume(Which::One(id)),
                Request::Resume(Which::All),
                Request::Queue { name: None, start: true },
                Request::Queue { name: Some("Night".into()), start: false },
            ]
        );
    }

    #[test]
    fn a_refusal_is_the_tools_error_with_what_to_try() {
        let mut tools = recorder();
        let refused = result_of(&mut tools, "remove_download", json!({ "id": 1 }));
        assert_eq!(refused["isError"], true);
        assert_eq!(refused["content"][0]["text"], "download 1 is running");
        let no_queue = result_of(&mut tools, "start_queue", json!({ "queue": "Day" }));
        assert!(no_queue["content"][0]["text"].as_str().unwrap().contains("list_downloads"));
    }

    #[test]
    fn protocol_errors_get_json_rpc_errors() {
        let mut tools = recorder();
        let input = "not json\n\n[]\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"resources/list\"}\n\
            {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"format_disk\"}}\n\
            {\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"ping\"}\n\
            {\"jsonrpc\":\"2.0\",\"id\":4,\"result\":{}}\n";
        let mut output = Vec::new();
        serve(input.as_bytes(), &mut output, &mut tools, "1").unwrap();
        let answers: Vec<Value> =
            String::from_utf8(output).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let codes: Vec<Value> = answers.iter().map(|a| a["error"]["code"].clone()).collect();
        assert_eq!(
            codes,
            [json!(PARSE_ERROR), json!(INVALID_REQUEST), json!(METHOD_NOT_FOUND), json!(INVALID_PARAMS), Value::Null]
        );
        assert_eq!(answers[4], json!({ "jsonrpc": "2.0", "id": 3, "result": {} }));
    }

    #[test]
    fn a_batch_is_answered_as_a_batch() {
        let mut tools = recorder();
        let batch = json!([
            initialize("2025-03-26"),
            { "jsonrpc": "2.0", "method": "notifications/initialized" },
            { "jsonrpc": "2.0", "id": 9, "method": "ping" },
        ]);
        let answers = talk(&mut tools, &[batch]);
        let ids: Vec<Value> = answers[0].as_array().unwrap().iter().map(|a| a["id"].clone()).collect();
        assert_eq!(ids, [json!(0), json!(9)]);
    }
}
