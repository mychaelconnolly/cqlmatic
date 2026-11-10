//! Pure normalization of saved Falcon MCP search results.

use std::{collections::HashSet, fmt};

use serde::de::{DeserializeSeed, Deserializer, Error as _, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};

use crate::{
    contracts::Diagnostic,
    network::{flagged, incomplete_flag, is_completeness_flag},
    sha256_hex,
};

pub const MAX_RESULT_BYTES: usize = 8 * 1024 * 1024;
// Retain the oversized sentinel byte so rejected captures can be replayed unchanged.
const MAX_CAPTURE_BYTES: usize = MAX_RESULT_BYTES + 1;

#[derive(Debug, Clone, PartialEq)]
pub struct Normalized {
    pub rows: Vec<Map<String, Value>>,
    pub trace_id: Option<String>,
    pub raw_sha256: String,
}

pub fn normalize(raw: &[u8]) -> Result<Normalized, Diagnostic> {
    if raw.len() > MAX_RESULT_BYTES {
        return Err(Diagnostic::error(
            "MCP_TOO_LARGE",
            "MCP search result exceeds the maximum size",
        ));
    }

    let value = parse_repeated_json(raw, "MCP search result must be valid UTF-8 JSON")?;
    let object = value
        .as_object()
        .ok_or_else(|| shape("MCP search result must be an object"))?;
    let body = if object.contains_key("content") {
        parse_envelope(object)?
    } else {
        value
    };

    parse_result(body, raw)
}

fn parse_envelope(object: &Map<String, Value>) -> Result<Value, Diagnostic> {
    // structuredContent is optional; when present it must equal the parsed text payload.
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "content" | "isError" | "structuredContent"))
    {
        return Err(shape("MCP text envelope has unexpected fields"));
    }
    let is_error = match object.get("isError") {
        None => false,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| shape("MCP text envelope isError must be a boolean"))?,
    };
    let content = object["content"]
        .as_array()
        .filter(|items| items.len() == 1)
        .ok_or_else(|| shape("MCP text envelope must contain one text item"))?;
    let item = content[0]
        .as_object()
        .filter(|item| item.len() == 2 && item.get("type").and_then(Value::as_str) == Some("text"))
        .ok_or_else(|| shape("MCP text envelope must contain a text item"))?;
    let text = item["text"]
        .as_str()
        .ok_or_else(|| shape("MCP text envelope text must be a string"))?;

    if is_error {
        if let Ok(Value::Object(inner)) = serde_json::from_str::<Value>(text) {
            parse_unique_json(
                text.as_bytes(),
                "MCP text envelope does not contain a JSON object",
            )?;
            if inner.contains_key("content") || inner.contains_key("isError") {
                return Err(shape("Nested MCP text envelopes are not supported"));
            }
        }
        if has_deadline(text) {
            return Err(Diagnostic::error(
                "MCP_DEADLINE",
                "MCP search tool reported a timeout",
            ));
        }
        return Err(Diagnostic::error(
            "MCP_TOOL_ERROR",
            format!(
                "MCP search tool error: {}",
                text.chars().take(300).collect::<String>()
            ),
        ));
    }

    let body = parse_unique_json(
        text.as_bytes(),
        "MCP text envelope does not contain a JSON object",
    )?;
    let inner = body
        .as_object()
        .ok_or_else(|| shape("MCP text envelope does not contain a JSON object"))?;
    if inner.contains_key("content") || inner.contains_key("isError") {
        return Err(shape("Nested MCP text envelopes are not supported"));
    }
    if let Some(structured) = object.get("structuredContent")
        && *structured != body
    {
        return Err(shape(
            "MCP structuredContent does not match the text content",
        ));
    }
    Ok(body)
}

fn parse_result(value: Value, raw: &[u8]) -> Result<Normalized, Diagnostic> {
    let mut object = match value {
        Value::Object(object) => object,
        _ => return Err(shape("MCP search result must be an object")),
    };

    let errors = ["error", "errors"]
        .into_iter()
        .filter_map(|key| object.get(key))
        .chain(
            ["metaData", "metadata"]
                .into_iter()
                .filter_map(|key| object.get(key).and_then(Value::as_object))
                .flat_map(|metadata| {
                    ["error", "errors"]
                        .into_iter()
                        .filter_map(|key| metadata.get(key))
                }),
        )
        .collect::<Vec<_>>();
    if object.values().filter_map(Value::as_str).any(has_deadline)
        || errors.iter().any(|value| error_has_deadline(value))
    {
        return Err(Diagnostic::error(
            "MCP_DEADLINE",
            "MCP search tool reported a timeout",
        ));
    }
    if errors.iter().any(|value| !value.is_null()) {
        return Err(Diagnostic::error(
            "MCP_TOOL_ERROR",
            "MCP search tool reported an error",
        ));
    }

    for (key, value) in &object {
        if is_completeness_flag(key) && incomplete_flag(value) {
            return Err(Diagnostic::error(
                "MCP_INCOMPLETE",
                format!("MCP search result indicates incomplete results: {key}"),
            ));
        }
    }
    for key in ["metaData", "metadata"] {
        if let Some(value) = object.get(key) {
            value
                .as_object()
                .ok_or_else(|| shape("MCP search result metadata must be an object"))?;
            if flagged(value)
                || value
                    .pointer("/filterQuery/isLive")
                    .is_some_and(incomplete_flag)
            {
                return Err(Diagnostic::error(
                    "MCP_INCOMPLETE",
                    "MCP search result metadata indicates incomplete results",
                ));
            }
        }
    }

    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "result" | "trace_id" | "metaData" | "metadata" | "error" | "errors"
        ) && !is_completeness_flag(key)
    }) {
        return Err(shape("MCP search result has unknown top-level fields"));
    }

    let trace_id = match object.remove("trace_id") {
        None => None,
        Some(Value::String(trace_id)) => Some(trace_id),
        Some(_) => return Err(shape("MCP search result trace_id must be a string")),
    };
    let rows = match object.remove("result") {
        Some(Value::Array(rows)) => rows,
        _ => return Err(shape("MCP search result must have a result array")),
    }
    .into_iter()
    .map(|row| match row {
        Value::Object(row) => Ok(row),
        _ => Err(shape("MCP search result contains a non-object row")),
    })
    .collect::<Result<Vec<_>, _>>()?;
    Ok(Normalized {
        rows,
        trace_id,
        raw_sha256: sha256_hex(raw),
    })
}

fn error_has_deadline(value: &Value) -> bool {
    match value {
        Value::String(text) => has_deadline(text),
        Value::Array(values) => values.iter().any(error_has_deadline),
        Value::Object(values) => values.values().any(error_has_deadline),
        _ => false,
    }
}

fn has_deadline(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    ["timeout", "timed out", "deadline"]
        .iter()
        .any(|word| lower.contains(word))
}

fn shape(message: &str) -> Diagnostic {
    Diagnostic::error("MCP_SHAPE", message)
}

/// Parse a saved MCP result file into one JSON value, accepting identical host-rendered repeats
/// (see `parse_repeated_json`). Callers that keep the value (for example `assess`) must still pass it
/// through [`normalize`] before trusting it.
pub fn parse_saved_result(raw: &[u8]) -> Result<Value, Diagnostic> {
    if raw.len() > MAX_RESULT_BYTES {
        return Err(Diagnostic::error(
            "MCP_TOO_LARGE",
            "MCP search result exceeds the maximum size",
        ));
    }
    parse_repeated_json(raw, "MCP search result must be valid UTF-8 JSON")
}

/// Agent hosts can render one MCP result more than once (GitHub Copilot CLI shows the text content
/// followed by the identical structuredContent). Accept whitespace-separated repeats only when every
/// copy is identical; any difference, or any copy failing the duplicate-key check, fails closed.
fn parse_repeated_json(raw: &[u8], message: &str) -> Result<Value, Diagnostic> {
    let mut stream = serde_json::Deserializer::from_slice(raw).into_iter::<serde::de::IgnoredAny>();
    let mut spans = Vec::new();
    let mut start = 0;
    while let Some(item) = stream.next() {
        item.map_err(|_| shape(message))?;
        let end = stream.byte_offset();
        spans.push((start, end));
        start = end;
    }
    let mut copies = spans
        .into_iter()
        .map(|(from, to)| parse_unique_json(&raw[from..to], message));
    let first = copies.next().ok_or_else(|| shape(message))??;
    for copy in copies {
        if copy? != first {
            return Err(shape("MCP search result repeats with different content"));
        }
    }
    Ok(first)
}

fn parse_unique_json(raw: &[u8], message: &str) -> Result<Value, Diagnostic> {
    let mut deserializer = serde_json::Deserializer::from_slice(raw);
    UniqueObjectKeys
        .deserialize(&mut deserializer)
        .and_then(|()| deserializer.end())
        .map_err(|_| shape(message))?;
    serde_json::from_slice(raw).map_err(|_| shape(message))
}

struct UniqueObjectKeys;

impl<'de> DeserializeSeed<'de> for UniqueObjectKeys {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for UniqueObjectKeys {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JSON with unique object keys")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let mut keys = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key) {
                return Err(A::Error::custom("duplicate JSON object key"));
            }
            map.next_value_seed(UniqueObjectKeys)?;
        }
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while seq.next_element_seed(UniqueObjectKeys)?.is_some() {}
        Ok(())
    }

    fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }

    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }

    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }

    fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<(), E> {
        Ok(())
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<(), E> {
        Ok(())
    }
}

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    discovery::{self, DiscoverReport, DiscoverRequest, Origin, Probe},
    environment::{self, MAX_PACK_BYTES, Pack},
    network::SearchBackend,
    storage,
};

const STATE_SCHEMA_VERSION: u32 = 2;
const DISCOVERY_ALGORITHM: &str = "bounded-discovery-v2";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivationIntent {
    request_sha256: String,
    run_id: String,
    predecessor_pack_sha256: Option<String>,
    pack_bytes: String,
    pack_sha256: String,
    receipt_bytes: String,
    receipt_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Answer {
    pub query: String,
    pub start: String,
    pub end: String,
    pub repository: String,
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    pub raw_hex: String,
    pub raw_sha256: String,
    pub ingested_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<Diagnostic>,
}

/// Unkeyed state detects corruption and inconsistent edits, not a party that can rewrite it
/// consistently; the agent is trusted to relay the MCP result bytes.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    #[serde(rename = "format")]
    pub format: String,
    pub schema_version: u32,
    discovery_algorithm: String,
    pub producer_version: String,
    pub created_at: String,
    pub request: DiscoverRequest,
    pub request_sha256: String,
    pub pack: PathBuf,
    pub receipt: PathBuf,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<Diagnostic>,
    pub answers: BTreeMap<String, Answer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    activation: Option<ActivationIntent>,
}

#[derive(Debug, Serialize)]
pub struct StepOutput {
    pub mode: &'static str,
    pub status: &'static str,
    pub answered: usize,
    pub max_queries: u32,
    pub probes: Vec<Value>,
    pub active_pack_updated: bool,
    pub diagnostics: Vec<Diagnostic>,
}
impl StepOutput {
    pub fn exit_code(&self) -> i32 {
        match self.status {
            "complete" => 0,
            "needs-probes" | "activation-blocked" => 3,
            _ => 2,
        }
    }
}

pub struct ReplayBackend<'a> {
    answers: &'a BTreeMap<String, Answer>,
}
impl<'a> ReplayBackend<'a> {
    pub fn new(answers: &'a BTreeMap<String, Answer>) -> Self {
        Self { answers }
    }
}
impl SearchBackend for ReplayBackend<'_> {
    fn start(
        &mut self,
        view: &str,
        query: &str,
        start: &str,
        end: &str,
        _: Instant,
    ) -> Result<String, Diagnostic> {
        let id = discovery::probe_id_for(view, query, start, end);
        let answer = self.answers.get(&id).ok_or_else(|| {
            Diagnostic::error(
                "MCP_PROBE_REQUIRED",
                "The next MCP measurement has no saved answer.",
            )
        })?;
        if answer.repository != view
            || answer.query != query
            || answer.start != start
            || answer.end != end
        {
            return Err(tampered());
        }
        if let Some(d) = &answer.diagnostic {
            return Err(d.clone());
        }
        if answer.outcome != "complete" {
            return Err(tampered());
        }
        Ok(id)
    }
    fn poll(&mut self, _: &str, job: &str, _: Instant) -> Result<Value, Diagnostic> {
        let answer = self.answers.get(job).ok_or_else(tampered)?;
        let rows = normalized_answer(answer)?
            .map_err(|_| tampered())?
            .rows
            .into_iter()
            .map(Value::Object)
            .collect::<Vec<_>>();
        Ok(
            json!({"done":true,"cancelled":false,"metaData":{"mcp":{"trace_id":answer.trace_id,"raw_sha256":answer.raw_sha256}},"events":rows}),
        )
    }
    fn stop(&mut self, _: &str, _: &str, _: Instant) -> Result<(), Diagnostic> {
        Err(Diagnostic::error(
            "MCP_LIFECYCLE",
            "The agent, not cqlmatic, owns the MCP search lifecycle.",
        ))
    }
    fn delegated_lifecycle(&self) -> bool {
        true
    }
    fn observed_at(&self, job: &str) -> Option<String> {
        self.answers.get(job).map(|a| a.ingested_at.clone())
    }
}

fn tampered() -> Diagnostic {
    Diagnostic::error(
        "MCP_STATE_TAMPERED",
        "MCP state identity or saved probe does not match the discovery request.",
    )
}
fn raw_hex(raw: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut encoded = String::with_capacity(raw.len() * 2);
    for byte in raw {
        write!(&mut encoded, "{byte:02x}").expect("writing to a string cannot fail");
    }
    encoded
}
fn normalized_answer(answer: &Answer) -> Result<Result<Normalized, Diagnostic>, Diagnostic> {
    if answer.raw_hex.len() > MAX_CAPTURE_BYTES * 2 || !answer.raw_hex.len().is_multiple_of(2) {
        return Err(tampered());
    }
    let raw = answer
        .raw_hex
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            std::str::from_utf8(pair)
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                .ok_or_else(tampered)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if sha256_hex(&raw) != answer.raw_sha256 {
        return Err(tampered());
    }
    Ok(normalize(&raw))
}
fn same_diagnostic(a: &Diagnostic, b: &Diagnostic) -> bool {
    serde_json::to_value(a).ok() == serde_json::to_value(b).ok()
}
fn path_identity(path: &Path) -> Result<PathBuf, Diagnostic> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|_| Diagnostic::error("LOCAL_PATH", "Cannot resolve the current directory."))?
            .join(path)
    };
    let path = absolute.as_path();
    let name = path
        .file_name()
        .ok_or_else(|| Diagnostic::error("LOCAL_PATH", "Discovery paths need filenames."))?;
    let mut parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| Diagnostic::error("LOCAL_PATH", "Invalid discovery directory."))?;
    let mut missing = Vec::new();
    while !parent.exists() {
        missing.push(
            parent
                .file_name()
                .ok_or_else(|| Diagnostic::error("LOCAL_PATH", "Invalid discovery directory."))?
                .to_os_string(),
        );
        parent = parent
            .parent()
            .ok_or_else(|| Diagnostic::error("LOCAL_PATH", "Invalid discovery directory."))?;
    }
    if !parent.is_dir() {
        return Err(Diagnostic::error(
            "LOCAL_PATH",
            "Discovery parent must be a directory.",
        ));
    }
    let mut result = fs::canonicalize(parent)
        .map_err(|_| Diagnostic::error("LOCAL_PATH", "Cannot resolve discovery directory."))?;
    for component in missing.iter().rev() {
        result.push(component);
    }
    result.push(name);
    Ok(result)
}
fn distinct_paths(paths: &[&Path]) -> Result<(), Diagnostic> {
    let mut unique = BTreeSet::new();
    for path in paths {
        let identity = if path.exists() {
            fs::canonicalize(path).map_err(|_| {
                Diagnostic::error("LOCAL_PATH", "Cannot resolve existing discovery path.")
            })?
        } else {
            path_identity(path)?
        };
        if !unique.insert(path_key(identity)) {
            return Err(Diagnostic::error(
                "CLI_INPUT",
                "State, pack and receipt paths must be different.",
            ));
        }
    }
    for path in paths {
        let lock = path_identity(&storage::writer_lock_path(path)?)?;
        if unique.contains(&path_key(lock)) {
            return Err(Diagnostic::error(
                "CLI_INPUT",
                "Discovery destinations must not overlap writer lock paths.",
            ));
        }
    }
    Ok(())
}
fn path_key(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        // New destination filenames cannot be canonicalized before they exist.
        PathBuf::from(path.to_string_lossy().to_lowercase())
    }
    #[cfg(not(windows))]
    {
        path
    }
}
fn digest(request: &DiscoverRequest) -> String {
    sha256_hex(&serde_json::to_vec(request).expect("discovery request serializes"))
}
fn state_bytes(state: &State) -> Result<Vec<u8>, Diagnostic> {
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|_| Diagnostic::error("LOCAL_SERIALIZATION", "Cannot serialize MCP state."))?;
    let limit = state_byte_limit();
    if bytes.len() as u64 + 1 > limit {
        return Err(Diagnostic::error(
            "LOCAL_SIZE",
            "MCP state exceeds its byte budget.",
        ));
    }
    Ok(bytes)
}
fn state_byte_limit() -> u64 {
    #[cfg(test)]
    {
        STATE_BYTE_LIMIT.with(|limit| limit.get())
    }
    #[cfg(not(test))]
    {
        MAX_PACK_BYTES
    }
}
fn store(writer: &storage::Writer, state: &State) -> Result<(), Diagnostic> {
    writer.commit_bytes(&state_bytes(state)?, MAX_PACK_BYTES)
}
fn run_id(state: &State) -> String {
    sha256_hex(
        &serde_json::to_vec(&(
            &state.request_sha256,
            &state.created_at,
            &state.pack,
            &state.receipt,
        ))
        .expect("run identity serializes"),
    )
}
fn validate_intent(state: &State, intent: &ActivationIntent) -> Result<(), Diagnostic> {
    if intent.request_sha256 != state.request_sha256
        || intent.run_id != run_id(state)
        || intent.pack_bytes.len() as u64 > MAX_PACK_BYTES
        || intent.receipt_bytes.len() as u64 > MAX_PACK_BYTES
        || !intent.pack_bytes.ends_with('\n')
        || !intent.receipt_bytes.ends_with('\n')
        || sha256_hex(intent.pack_bytes.as_bytes()) != intent.pack_sha256
        || sha256_hex(intent.receipt_bytes.as_bytes()) != intent.receipt_sha256
        || intent.predecessor_pack_sha256.as_ref().is_some_and(|hash| {
            hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    {
        return Err(tampered());
    }
    let pack: Pack = serde_json::from_str(&intent.pack_bytes).map_err(|_| tampered())?;
    pack.validate().map_err(|_| tampered())?;
    let receipt = parse_unique_json(intent.receipt_bytes.as_bytes(), "Invalid frozen receipt.")
        .map_err(|_| tampered())?;
    let report: DiscoverReport = serde_json::from_value(receipt.clone()).map_err(|_| tampered())?;
    if pack.environment != state.request.environment
        || report.environment != state.request.environment
        || report.status != "complete"
        || !report.active_pack_updated
        || receipt["mode"] != "mcp"
        || receipt["request_sha256"] != state.request_sha256
        || receipt["run_id"] != intent.run_id
        || report.captures.len() != state.answers.len()
    {
        return Err(tampered());
    }
    let mut seen = BTreeSet::new();
    for capture in report.captures {
        let id = discovery::probe_id(&state.request.environment.view, &capture.probe);
        let answer = state.answers.get(&id).ok_or_else(tampered)?;
        if !seen.insert(id)
            || answer.outcome != "complete"
            || elapsed(state, &answer.ingested_at).is_err()
            || capture.retrieved_at != answer.ingested_at
            || capture.receipt.query != answer.query
            || capture.receipt.start != answer.start
            || capture.receipt.end != answer.end
            || capture.receipt.completion != "complete"
            || capture
                .receipt
                .metadata
                .as_ref()
                .and_then(|v| v.pointer("/mcp/raw_sha256"))
                .and_then(Value::as_str)
                != Some(answer.raw_sha256.as_str())
        {
            return Err(tampered());
        }
    }
    Ok(())
}
fn optional_bytes(path: &Path) -> Result<Option<Vec<u8>>, Diagnostic> {
    match fs::symlink_metadata(path) {
        Ok(_) => storage::read_bytes(path, MAX_PACK_BYTES).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Diagnostic::error(
            "LOCAL_IO",
            format!("Cannot inspect discovery path: {e}"),
        )),
    }
}
fn read_state(path: &Path) -> Result<State, Diagnostic> {
    let bytes = storage::read_bytes(path, MAX_PACK_BYTES)?;
    let header: Value = serde_json::from_slice(&bytes)
        .map_err(|_| Diagnostic::error("MCP_STATE_TAMPERED", "Cannot decode MCP state."))?;
    if header.get("format").and_then(Value::as_str) == Some("cqlmatic-mcp-state")
        && (header.get("schema_version").and_then(Value::as_u64)
            != Some(STATE_SCHEMA_VERSION as u64)
            || header.get("discovery_algorithm").and_then(Value::as_str)
                != Some(DISCOVERY_ALGORITHM))
    {
        return Err(Diagnostic::error(
            "MCP_STATE_RESTART",
            "MCP state uses an incompatible discovery algorithm or schema; restart discovery with new state and receipt paths. Existing packs remain readable.",
        ));
    }
    let state: State = serde_json::from_slice(&bytes)
        .map_err(|_| Diagnostic::error("MCP_STATE_TAMPERED", "Cannot decode MCP state."))?;
    if state.format != "cqlmatic-mcp-state"
        || state.schema_version != STATE_SCHEMA_VERSION
        || state.discovery_algorithm != DISCOVERY_ALGORITHM
        || state.producer_version != env!("CARGO_PKG_VERSION")
        || !matches!(
            state.status.as_str(),
            "needs-probes" | "activation-blocked" | "activating" | "activated" | "failed"
        )
        || state.request_sha256 != digest(&state.request)
        || discovery::validate_request(&state.request).is_err()
        || environment::parse_time(&state.created_at).is_err()
    {
        return Err(tampered());
    }
    if !state.pack.is_absolute() || !state.receipt.is_absolute() {
        return Err(tampered());
    }
    if path_identity(&state.pack)? != state.pack || path_identity(&state.receipt)? != state.receipt
    {
        return Err(tampered());
    }
    distinct_paths(&[path, &state.pack, &state.receipt])?;
    if let Some(intent) = &state.activation {
        validate_intent(&state, intent)?;
    }
    if state.status == "activated" {
        let expected = state.pack_sha256.as_deref().ok_or_else(tampered)?;
        let intent = state.activation.as_ref().ok_or_else(tampered)?;
        let committed = storage::read_bytes(&state.pack, MAX_PACK_BYTES).map_err(|_| tampered())?;
        if expected != sha256_hex(&committed)
            || expected != intent.pack_sha256
            || storage::read_bytes(&state.receipt, MAX_PACK_BYTES)
                .map_or(true, |bytes| bytes != intent.receipt_bytes.as_bytes())
            || state.failure.is_some()
        {
            return Err(tampered());
        }
    } else if state.pack_sha256.is_some()
        || (state.status == "failed") != state.failure.is_some()
        || (state.status == "activating") != state.activation.is_some()
    {
        return Err(tampered());
    }
    for (id, answer) in &state.answers {
        if discovery::probe_id_for(
            &answer.repository,
            &answer.query,
            &answer.start,
            &answer.end,
        ) != *id
            || answer.repository != state.request.environment.view
            || environment::parse_time(&answer.ingested_at).is_err()
        {
            return Err(tampered());
        }
        // Replay ingestion's deadline precedence while still checking the raw capture hash.
        let normalized = normalized_answer(answer)?;
        match elapsed(&state, &answer.ingested_at).and(normalized) {
            Ok(value)
                if value.rows.len() >= 10_000
                    && answer.outcome == "failed"
                    && answer
                        .diagnostic
                        .as_ref()
                        .is_some_and(|d| d.code == "SEARCH_ROW_LIMIT")
                    && answer.trace_id.is_none() => {}
            Ok(value)
                if value.rows.len() < 10_000
                    && answer.outcome == "complete"
                    && answer.diagnostic.is_none()
                    && answer.trace_id == value.trace_id => {}
            Err(d)
                if answer.outcome == "failed"
                    && answer
                        .diagnostic
                        .as_ref()
                        .is_some_and(|saved| same_diagnostic(saved, &d))
                    && answer.trace_id.is_none() => {}
            _ => return Err(tampered()),
        }
    }
    Ok(state)
}
fn successes(state: &State) -> Result<BTreeMap<String, Vec<Value>>, Diagnostic> {
    state
        .answers
        .iter()
        .filter(|(_, answer)| answer.outcome == "complete")
        .map(|(id, answer)| {
            let rows = normalized_answer(answer)?
                .map_err(|_| tampered())?
                .rows
                .into_iter()
                .map(Value::Object)
                .collect();
            Ok((id.clone(), rows))
        })
        .collect()
}
fn pending(state: &State) -> Result<Vec<Probe>, Diagnostic> {
    let remaining = state
        .request
        .max_queries
        .saturating_sub(state.answers.len() as u32);
    Ok(discovery::frontier(&state.request, &successes(state)?)?
        .into_iter()
        .take(remaining as usize)
        .collect())
}
fn wire_probes(state: &State, probes: Vec<Probe>) -> Vec<Value> {
    probes
        .into_iter()
        .map(|p| {
            json!({"probe_id":discovery::probe_id(&state.request.environment.view,&p),"kind":p.kind,"source":p.source,"fields":p.fields,"field":p.field,"mcp":{"tool":"falcon_search_ngsiem","arguments":{"query_string":p.query,"start":p.start,"end":p.end,"repository":state.request.environment.view}}})
        })
        .collect()
}
fn progress(
    state: &State,
    status: &'static str,
    probes: Vec<Probe>,
    active_pack_updated: bool,
    diagnostics: Vec<Diagnostic>,
) -> StepOutput {
    StepOutput {
        mode: "mcp",
        status,
        answered: state.answers.len(),
        max_queries: state.request.max_queries,
        probes: wire_probes(state, probes),
        active_pack_updated,
        diagnostics,
    }
}
fn elapsed(state: &State, now: &str) -> Result<Duration, Diagnostic> {
    let created = environment::parse_time(&state.created_at).map_err(|_| tampered())?;
    let now = environment::parse_time(now).map_err(|_| tampered())?;
    if now < created {
        return Err(tampered());
    }
    let seconds = (now - created).as_seconds_f64();
    if seconds >= state.request.max_seconds as f64 {
        return Err(Diagnostic::error(
            "MCP_DEADLINE",
            "Discovery wall-clock budget expired before the last answer was ingested.",
        ));
    }
    Ok(Duration::from_secs_f64(
        state.request.max_seconds as f64 - seconds,
    ))
}

pub fn init(
    request: DiscoverRequest,
    state_path: &Path,
    pack_path: &Path,
    receipt_path: &Path,
) -> Result<StepOutput, Diagnostic> {
    discovery::validate_request(&request)?;
    distinct_paths(&[state_path, pack_path, receipt_path])?;
    let state_path = path_identity(state_path)?;
    let pack_path = path_identity(pack_path)?;
    let receipt_path = path_identity(receipt_path)?;
    if state_path.exists() || receipt_path.exists() {
        return Err(Diagnostic::error(
            "CLI_INPUT",
            "MCP initialization requires new state and receipt paths.",
        ));
    }
    // Every MCP writer takes locks in state -> pack -> receipt order.
    let writer = storage::Writer::acquire(&state_path)?;
    let pack_writer = storage::Writer::acquire(&pack_path)?;
    let _receipt_writer = storage::Writer::acquire(&receipt_path)?;
    if pack_writer
        .existing()?
        .is_some_and(|p| p.environment != request.environment)
    {
        return Err(Diagnostic::error(
            "PACK_ENVIRONMENT_MISMATCH",
            "Active pack belongs to a different cloud/account/view.",
        ));
    }
    if receipt_path.exists() {
        return Err(Diagnostic::error(
            "CLI_INPUT",
            "MCP receipt path must not already exist.",
        ));
    }
    let created_at = discovery::timestamp();
    let state = State {
        format: "cqlmatic-mcp-state".into(),
        schema_version: STATE_SCHEMA_VERSION,
        discovery_algorithm: DISCOVERY_ALGORITHM.into(),
        producer_version: env!("CARGO_PKG_VERSION").into(),
        created_at,
        request_sha256: digest(&request),
        request,
        pack: pack_path,
        receipt: receipt_path,
        status: "needs-probes".into(),
        pack_sha256: None,
        failure: None,
        answers: BTreeMap::new(),
        activation: None,
    };
    let probes = pending(&state)?;
    if probes.is_empty() {
        return Err(Diagnostic::error(
            "MCP_PROBE_REQUIRED",
            "Discovery has no initial probes.",
        ));
    }
    elapsed(&state, &discovery::timestamp())?;
    writer.create_new_bytes(&state_bytes(&state)?, MAX_PACK_BYTES)?;
    Ok(progress(&state, "needs-probes", probes, false, vec![]))
}

fn fail_state(
    writer: &storage::Writer,
    state: &mut State,
    diagnostic: Diagnostic,
) -> Result<StepOutput, Diagnostic> {
    state.status = "failed".into();
    state.failure = Some(diagnostic.clone());
    store(writer, state)?;
    Ok(progress(state, "failed", vec![], false, vec![diagnostic]))
}
fn activate(
    writer: &storage::Writer,
    state: &mut State,
    candidate: Pack,
    mut report: DiscoverReport,
) -> Result<StepOutput, Diagnostic> {
    let pack_writer = match storage::Writer::acquire(&state.pack) {
        Ok(writer) => writer,
        Err(d) if d.code == "PACK_BUSY" => {
            state.status = "activation-blocked".into();
            store(writer, state)?;
            return Ok(progress(
                state,
                "activation-blocked",
                vec![],
                false,
                vec![d],
            ));
        }
        Err(d) => return fail_state(writer, state, d),
    };
    let old = match pack_writer.existing() {
        Ok(old) => old,
        Err(d) => return fail_state(writer, state, d),
    };
    if old
        .as_ref()
        .is_some_and(|p| p.environment != state.request.environment)
    {
        return fail_state(
            writer,
            state,
            Diagnostic::error(
                "PACK_ENVIRONMENT_MISMATCH",
                "Active pack belongs to a different cloud/account/view.",
            ),
        );
    }
    let merged = match old {
        Some(old) => environment::merge(&old, &candidate),
        None => Ok(candidate),
    };
    let merged = match merged {
        Ok(pack) => pack,
        Err(d) => return fail_state(writer, state, d),
    };
    let receipt_writer = match storage::Writer::acquire(&state.receipt) {
        Ok(writer) => writer,
        Err(d) if d.code == "PACK_BUSY" => {
            state.status = "activation-blocked".into();
            store(writer, state)?;
            return Ok(activation_blocked(state, d));
        }
        Err(d) => return fail_state(writer, state, d),
    };
    if optional_bytes(&state.receipt)?.is_some() {
        return fail_state(
            writer,
            state,
            Diagnostic::error(
                "LOCAL_IO",
                "MCP receipt path must remain absent before activation.",
            ),
        );
    }
    let predecessor = optional_bytes(&state.pack)?.map(|bytes| sha256_hex(&bytes));
    report.active_pack_updated = true;
    let mut receipt = serde_json::to_value(&report)
        .map_err(|_| Diagnostic::error("LOCAL_SERIALIZATION", "Cannot serialize MCP receipt."))?;
    receipt["mode"] = json!("mcp");
    receipt["request_sha256"] = json!(state.request_sha256);
    receipt["run_id"] = json!(run_id(state));
    let pack_bytes = frozen_json(&merged)?;
    let receipt_bytes = frozen_json(&receipt)?;
    let intent = ActivationIntent {
        request_sha256: state.request_sha256.clone(),
        run_id: run_id(state),
        predecessor_pack_sha256: predecessor,
        pack_sha256: sha256_hex(pack_bytes.as_bytes()),
        pack_bytes,
        receipt_sha256: sha256_hex(receipt_bytes.as_bytes()),
        receipt_bytes,
    };
    validate_intent(state, &intent)?;
    state.status = "activating".into();
    state.activation = Some(intent);
    // Preflight both serialized states before the first activation side effect.
    let intent_bytes = state_bytes(state)?;
    completed_state_bytes(state)?;
    activation_checkpoint("before-intent")?;
    if let Err(d) = writer.commit_bytes(&intent_bytes, MAX_PACK_BYTES) {
        // A visible intent with unconfirmed directory durability must never authorize a pack write.
        return Ok(activation_blocked(state, d));
    }
    activation_checkpoint("after-intent")?;
    finish_activation(writer, &pack_writer, &receipt_writer, state)
}
fn frozen_json(value: &impl Serialize) -> Result<String, Diagnostic> {
    let mut bytes = serde_json::to_string_pretty(value).map_err(|_| {
        Diagnostic::error(
            "LOCAL_SERIALIZATION",
            "Cannot serialize activation payload.",
        )
    })?;
    bytes.push('\n');
    if bytes.len() as u64 > MAX_PACK_BYTES {
        return Err(Diagnostic::error(
            "LOCAL_SIZE",
            "Activation payload exceeds its byte budget.",
        ));
    }
    Ok(bytes)
}
fn completed_state_bytes(state: &mut State) -> Result<Vec<u8>, Diagnostic> {
    let target = state
        .activation
        .as_ref()
        .ok_or_else(tampered)?
        .pack_sha256
        .clone();
    let prior_status = std::mem::replace(&mut state.status, "activated".into());
    let prior_hash = state.pack_sha256.replace(target);
    let bytes = state_bytes(state);
    state.status = prior_status;
    state.pack_sha256 = prior_hash;
    bytes
}
fn visible_target(state: &State) -> bool {
    state.activation.as_ref().is_some_and(|intent| {
        optional_bytes(&state.pack)
            .ok()
            .flatten()
            .is_some_and(|bytes| sha256_hex(&bytes) == intent.pack_sha256)
    })
}
fn activation_blocked(state: &State, diagnostic: Diagnostic) -> StepOutput {
    progress(
        state,
        "activation-blocked",
        vec![],
        visible_target(state),
        vec![diagnostic],
    )
}
fn activation_conflict(state: &State, message: &str) -> StepOutput {
    progress(
        state,
        "failed",
        vec![],
        visible_target(state),
        vec![Diagnostic::error("MCP_ACTIVATION_CONFLICT", message)],
    )
}
fn recover_activation(
    writer: &storage::Writer,
    state: &mut State,
) -> Result<StepOutput, Diagnostic> {
    let pack_writer = match storage::Writer::acquire(&state.pack) {
        Ok(writer) => writer,
        Err(d) => return Ok(activation_blocked(state, d)),
    };
    let receipt_writer = match storage::Writer::acquire(&state.receipt) {
        Ok(writer) => writer,
        Err(d) => return Ok(activation_blocked(state, d)),
    };
    if let Err(d) = writer.confirm_durable() {
        return Ok(activation_blocked(state, d));
    }
    finish_activation(writer, &pack_writer, &receipt_writer, state)
}
fn finish_activation(
    writer: &storage::Writer,
    pack_writer: &storage::Writer,
    receipt_writer: &storage::Writer,
    state: &mut State,
) -> Result<StepOutput, Diagnostic> {
    let completed_bytes = completed_state_bytes(state)?;
    let intent = state.activation.as_ref().ok_or_else(tampered)?;
    let current_pack = optional_bytes(&state.pack)?;
    let current_hash = current_pack.as_ref().map(|bytes| sha256_hex(bytes));
    let target_visible = current_hash.as_deref() == Some(intent.pack_sha256.as_str());
    if !target_visible && current_hash != intent.predecessor_pack_sha256 {
        return Ok(activation_conflict(
            state,
            "Active pack differs from both the frozen predecessor and activation target. No files were replaced on this attempt; prior activation outcome is unknown.",
        ));
    }
    let receipt = optional_bytes(&state.receipt)?;
    if receipt
        .as_ref()
        .is_some_and(|bytes| bytes != intent.receipt_bytes.as_bytes())
    {
        return Ok(activation_conflict(
            state,
            "Receipt differs from the frozen activation receipt. No files were replaced on this attempt.",
        ));
    }
    if receipt.is_some() && !target_visible {
        return Ok(activation_conflict(
            state,
            "Receipt exists before its frozen pack target. No files were replaced on this attempt; prior activation outcome is unknown.",
        ));
    }
    activation_checkpoint("before-pack")?;
    let committed = if target_visible {
        pack_writer.confirm_durable()
    } else {
        pack_writer.commit_frozen_bytes(intent.pack_bytes.as_bytes(), MAX_PACK_BYTES)
    };
    if let Err(d) = committed {
        return Ok(activation_blocked(state, d));
    }
    activation_checkpoint("after-pack")?;
    activation_checkpoint("before-receipt")?;
    let committed = if receipt.is_some() {
        receipt_writer.confirm_durable()
    } else {
        receipt_writer.commit_frozen_bytes(intent.receipt_bytes.as_bytes(), MAX_PACK_BYTES)
    };
    if let Err(d) = committed {
        return Ok(activation_blocked(state, d));
    }
    activation_checkpoint("after-receipt")?;
    activation_checkpoint("before-activated")?;
    state.pack_sha256 = Some(intent.pack_sha256.clone());
    state.status = "activated".into();
    if let Err(d) = writer.commit_bytes(&completed_bytes, MAX_PACK_BYTES) {
        return Ok(activation_blocked(state, d));
    }
    activation_checkpoint("after-activated")?;
    Ok(progress(state, "complete", vec![], true, vec![]))
}

#[cfg(test)]
thread_local! {
    static ACTIVATION_FAULT: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
    static STATE_BYTE_LIMIT: std::cell::Cell<u64> = const { std::cell::Cell::new(MAX_PACK_BYTES) };
}
fn activation_checkpoint(_stage: &str) -> Result<(), Diagnostic> {
    #[cfg(test)]
    if ACTIVATION_FAULT.with(|fault| {
        if fault.get() == Some(_stage) {
            fault.set(None);
            return true;
        }
        false
    }) {
        return Err(Diagnostic::error(
            "TEST_INTERRUPTED",
            "Injected activation interruption.",
        ));
    }
    Ok(())
}
fn advance(writer: &storage::Writer, state: &mut State) -> Result<StepOutput, Diagnostic> {
    if state.status == "activated" {
        if let Err(d) = writer.confirm_durable() {
            return Ok(activation_blocked(state, d));
        }
        return Ok(progress(state, "complete", vec![], true, vec![]));
    }
    if state.status == "activating" {
        return recover_activation(writer, state);
    }
    if state.status == "failed" {
        let diagnostic = state
            .failure
            .clone()
            .expect("failed state has a diagnostic");
        return Ok(progress(state, "failed", vec![], false, vec![diagnostic]));
    }
    let remaining = if let Some(last_ingest) = state.answers.values().max_by_key(|answer| {
        environment::parse_time(&answer.ingested_at).expect("state times were validated")
    }) {
        if let Err(d) = elapsed(state, &last_ingest.ingested_at) {
            return fail_state(writer, state, d);
        }
        Duration::from_secs(state.request.max_seconds)
    } else {
        match elapsed(state, &discovery::timestamp()) {
            Ok(remaining) => remaining,
            Err(d) => return fail_state(writer, state, d),
        }
    };
    let mut replay = ReplayBackend::new(&state.answers);
    let (candidate, report) =
        discovery::execute_with_origin(&state.request, &mut replay, Origin::Mcp, remaining);
    if let Some(candidate) = candidate {
        return activate(writer, state, candidate, report);
    }
    let diagnostic = report
        .diagnostics
        .into_iter()
        .next()
        .expect("discovery failure has a diagnostic");
    if diagnostic.code == "MCP_PROBE_REQUIRED" {
        match pending(state) {
            Ok(probes) if !probes.is_empty() => {
                // A timely earlier answer does not extend the budget for later measurements.
                if let Err(d) = elapsed(state, &discovery::timestamp()) {
                    return fail_state(writer, state, d);
                }
                state.status = "needs-probes".into();
                store(writer, state)?;
                return Ok(progress(state, "needs-probes", probes, false, vec![]));
            }
            Ok(_) => {}
            Err(d) => return fail_state(writer, state, d),
        }
        return fail_state(
            writer,
            state,
            Diagnostic::error("DISCOVERY_EVIDENCE", "Discovery query budget exhausted."),
        );
    }
    fail_state(writer, state, diagnostic)
}
pub fn step(state_path: &Path, answers: &[(String, PathBuf)]) -> Result<StepOutput, Diagnostic> {
    let writer = storage::Writer::acquire(state_path)?;
    let mut state = read_state(state_path)?;
    if state.status == "failed" {
        return advance(&writer, &mut state);
    }
    if answers.iter().any(|(id, _)| state.answers.contains_key(id)) {
        return Err(Diagnostic::error(
            "MCP_ALREADY_ANSWERED",
            "This probe was already answered.",
        ));
    }
    if !answers.is_empty() && state.status != "needs-probes" {
        return Err(Diagnostic::error(
            "MCP_UNKNOWN_PROBE",
            "No new probes can be answered in this state.",
        ));
    }
    if answers.is_empty() && matches!(state.status.as_str(), "activated" | "activating") {
        return advance(&writer, &mut state);
    }
    let allowed = pending(&state)?
        .iter()
        .map(|p| discovery::probe_id(&state.request.environment.view, p))
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    for (id, _) in answers {
        if state.answers.contains_key(id) || !seen.insert(id.clone()) {
            return Err(Diagnostic::error(
                "MCP_ALREADY_ANSWERED",
                "This probe was already answered.",
            ));
        }
        if !allowed.contains(id) {
            return Err(Diagnostic::error(
                "MCP_UNKNOWN_PROBE",
                "The probe was not emitted in the current batch.",
            ));
        }
    }
    let probes = discovery::frontier(&state.request, &successes(&state)?)?;
    for (id, path) in answers {
        let probe = probes
            .iter()
            .find(|p| discovery::probe_id(&state.request.environment.view, p) == *id)
            .expect("allowed probe exists");
        let raw = storage::read_bytes(path, MAX_CAPTURE_BYTES as u64)?;
        let ingested_at = discovery::timestamp();
        let remaining = elapsed(&state, &ingested_at);
        let normalized = remaining.and_then(|_| {
            normalize(&raw).and_then(|value| {
                if value.rows.len() >= 10_000 {
                    Err(Diagnostic::error(
                        "SEARCH_ROW_LIMIT",
                        "Result shape or row count exceeds the evidence budget.",
                    ))
                } else {
                    Ok(value)
                }
            })
        });
        let (outcome, trace_id, diagnostic) = match normalized {
            Ok(value) => ("complete".into(), value.trace_id, None),
            Err(d) => ("failed".into(), None, Some(d)),
        };
        let failure = diagnostic.clone();
        state.answers.insert(
            id.clone(),
            Answer {
                query: probe.query.clone(),
                start: probe.start.clone(),
                end: probe.end.clone(),
                repository: state.request.environment.view.clone(),
                outcome,
                trace_id,
                raw_hex: raw_hex(&raw),
                raw_sha256: sha256_hex(&raw),
                ingested_at,
                diagnostic,
            },
        );
        store(&writer, &state)?;
        if let Some(d) = failure {
            return fail_state(&writer, &mut state, d);
        }
    }
    advance(&writer, &mut state)
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Run {
        directory: PathBuf,
        state: PathBuf,
        pack: PathBuf,
        receipt: PathBuf,
    }
    impl Drop for Run {
        fn drop(&mut self) {
            STATE_BYTE_LIMIT.with(|limit| limit.set(MAX_PACK_BYTES));
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
    fn fixture(probe: &Value) -> &'static str {
        match (probe["kind"].as_str().unwrap(), probe["field"].as_str()) {
            ("volume", _) | ("tag-detect", Some("event_simpleName")) => {
                include_str!("../examples/mcp/count.json")
            }
            ("tag-detect", _) => r#"{"result":[{"events":"0"}]}"#,
            ("dataset-facet", _) => include_str!("../examples/mcp/dataset.json"),
            ("fieldset", _) => include_str!("../examples/mcp/fieldset.json"),
            ("population", _) => include_str!("../examples/mcp/population.json"),
            ("value-facet", _) => include_str!("../examples/mcp/value.json"),
            ("record-kind-facet", Some("#type")) => include_str!("../examples/mcp/kind.json"),
            ("record-kind-facet", Some("#event_simpleName")) => {
                include_str!("../examples/mcp/simple-name.json")
            }
            _ => panic!("unexpected probe: {probe}"),
        }
    }
    fn ready(predecessor: bool) -> Run {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "cqlmatic-recovery-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let run = Run {
            state: directory.join("state.json"),
            pack: directory.join("pack.json"),
            receipt: directory.join("receipt.json"),
            directory,
        };
        let request: DiscoverRequest =
            serde_json::from_str(include_str!("../examples/mcp/discover.request.json")).unwrap();
        if predecessor {
            let pack = Pack::new(
                request.environment.clone(),
                "2026-01-01T00:00:00Z".into(),
                Default::default(),
            )
            .unwrap();
            storage::Writer::acquire(&run.pack)
                .unwrap()
                .commit(&pack)
                .unwrap();
        }
        let mut output = init(request, &run.state, &run.pack, &run.receipt).unwrap();
        let held = storage::Writer::acquire(&run.pack).unwrap();
        while output.status == "needs-probes" {
            let answers = output
                .probes
                .iter()
                .enumerate()
                .map(|(i, probe)| {
                    let path = run.directory.join(format!("answer-{i}.json"));
                    fs::write(&path, fixture(probe)).unwrap();
                    (probe["probe_id"].as_str().unwrap().into(), path)
                })
                .collect::<Vec<_>>();
            output = step(&run.state, &answers).unwrap();
        }
        assert_eq!(output.status, "activation-blocked");
        drop(held);
        run
    }
    fn interrupt(run: &Run, stage: &'static str) {
        ACTIVATION_FAULT.with(|fault| fault.set(Some(stage)));
        assert_eq!(step(&run.state, &[]).unwrap_err().code, "TEST_INTERRUPTED");
    }
    fn frozen(run: &Run) -> ActivationIntent {
        read_state(&run.state).unwrap().activation.unwrap()
    }
    fn assert_complete(run: &Run, intent: &ActivationIntent) {
        let output = step(&run.state, &[]).unwrap();
        assert_eq!(output.status, "complete", "{:?}", output.diagnostics);
        assert!(output.active_pack_updated);
        assert_eq!(fs::read(&run.pack).unwrap(), intent.pack_bytes.as_bytes());
        assert_eq!(
            fs::read(&run.receipt).unwrap(),
            intent.receipt_bytes.as_bytes()
        );
        assert_eq!(
            sha256_hex(&fs::read(&run.pack).unwrap()),
            intent.pack_sha256
        );
        assert_eq!(
            sha256_hex(&fs::read(&run.receipt).unwrap()),
            intent.receipt_sha256
        );
    }

    #[test]
    fn interruptions_at_every_activation_transition_recover_frozen_bytes() {
        for predecessor in [false, true] {
            for stage in [
                "before-intent",
                "after-intent",
                "before-pack",
                "after-pack",
                "before-receipt",
                "after-receipt",
                "before-activated",
                "after-activated",
            ] {
                let run = ready(predecessor);
                let before = optional_bytes(&run.pack).unwrap();
                interrupt(&run, stage);
                if stage == "before-intent" {
                    assert_eq!(optional_bytes(&run.pack).unwrap(), before);
                    assert!(!run.receipt.exists());
                    assert!(read_state(&run.state).unwrap().activation.is_none());
                    assert_eq!(step(&run.state, &[]).unwrap().status, "complete");
                    continue;
                }
                let intent = frozen(&run);
                assert_eq!(
                    intent.predecessor_pack_sha256,
                    before.map(|bytes| sha256_hex(&bytes))
                );
                if let Some(receipt) = optional_bytes(&run.receipt).unwrap() {
                    assert_eq!(receipt, intent.receipt_bytes.as_bytes());
                    assert!(!receipt.is_empty());
                }
                // Recovery must work without any original answer files.
                for entry in fs::read_dir(&run.directory).unwrap() {
                    let path = entry.unwrap().path();
                    if path
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("answer-")
                    {
                        fs::remove_file(path).unwrap();
                    }
                }
                assert_complete(&run, &intent);
            }
        }
    }

    #[test]
    fn intent_write_failures_never_mutate_the_pack() {
        for stage in ["before-rename", "directory-sync"] {
            let run = ready(true);
            let before = fs::read(&run.pack).unwrap();
            storage::inject_snapshot_fault(&run.state, stage);
            let output = step(&run.state, &[]).unwrap();
            assert_eq!(output.status, "activation-blocked");
            assert!(!output.active_pack_updated);
            assert_eq!(fs::read(&run.pack).unwrap(), before);
            assert!(!run.receipt.exists());
            if stage == "directory-sync" {
                assert_eq!(output.diagnostics[0].code, "PACK_DURABILITY");
                assert_complete(&run, &frozen(&run));
            } else {
                assert!(read_state(&run.state).unwrap().activation.is_none());
                assert_eq!(step(&run.state, &[]).unwrap().status, "complete");
            }
        }
    }

    #[test]
    fn failed_pack_receipt_and_final_state_writes_remain_recoverable() {
        for target in ["pack", "receipt", "state"] {
            for stage in ["before-rename", "directory-sync"] {
                let run = ready(false);
                interrupt(&run, "after-intent");
                let intent = frozen(&run);
                let state = read_state(&run.state).unwrap();
                let path = match target {
                    "pack" => &state.pack,
                    "receipt" => &state.receipt,
                    _ => &run.state,
                };
                storage::inject_snapshot_fault(path, stage);
                let output = step(&run.state, &[]).unwrap();
                assert_eq!(output.status, "activation-blocked", "{target} {stage}");
                let target_visible = target != "pack" || stage == "directory-sync";
                assert_eq!(
                    output.active_pack_updated, target_visible,
                    "{target} {stage}"
                );
                assert_ne!(output.exit_code(), 0);
                if stage == "directory-sync" {
                    assert_eq!(output.diagnostics[0].code, "PACK_DURABILITY");
                }
                assert_complete(&run, &intent);
            }
        }
    }

    #[test]
    fn recovery_refuses_unrelated_pack_and_receipt_replacements() {
        for target in ["pack", "receipt"] {
            let run = ready(true);
            interrupt(&run, "after-intent");
            let state = read_state(&run.state).unwrap();
            let path = if target == "pack" {
                &state.pack
            } else {
                &state.receipt
            };
            fs::write(path, b"unrelated replacement").unwrap();
            let before_pack = fs::read(&run.pack).unwrap();
            let before_receipt = optional_bytes(&run.receipt).unwrap();
            let output = step(&run.state, &[]).unwrap();
            assert_eq!(output.status, "failed");
            assert_eq!(output.diagnostics[0].code, "MCP_ACTIVATION_CONFLICT");
            assert_eq!(fs::read(&run.pack).unwrap(), before_pack);
            assert_eq!(optional_bytes(&run.receipt).unwrap(), before_receipt);
            assert_eq!(read_state(&run.state).unwrap().status, "activating");
        }
    }

    #[test]
    fn complete_state_size_is_preflighted_before_activation_side_effects() {
        let run = ready(true);
        let before = fs::read(&run.pack).unwrap();
        let state_before = fs::read(&run.state).unwrap();
        STATE_BYTE_LIMIT.with(|limit| limit.set(state_before.len() as u64 + 256));
        assert_eq!(step(&run.state, &[]).unwrap_err().code, "LOCAL_SIZE");
        assert_eq!(fs::read(&run.pack).unwrap(), before);
        assert_eq!(fs::read(&run.state).unwrap(), state_before);
        assert!(!run.receipt.exists());
    }

    #[test]
    fn final_state_metadata_is_included_in_size_preflight() {
        let run = ready(false);
        let before = fs::read(&run.state).unwrap();
        interrupt(&run, "after-intent");
        let state = read_state(&run.state).unwrap();
        let intent_limit = state_bytes(&state).unwrap().len() as u64 + 1;
        let intent = state.activation.unwrap();
        let candidate: Pack = serde_json::from_str(&intent.pack_bytes).unwrap();
        let report: DiscoverReport = serde_json::from_str(&intent.receipt_bytes).unwrap();
        fs::write(&run.state, &before).unwrap();
        let mut state = read_state(&run.state).unwrap();
        let writer = storage::Writer::acquire(&run.state).unwrap();
        STATE_BYTE_LIMIT.with(|limit| limit.set(intent_limit));
        assert_eq!(
            activate(&writer, &mut state, candidate, report)
                .unwrap_err()
                .code,
            "LOCAL_SIZE"
        );
        assert_eq!(fs::read(&run.state).unwrap(), before);
        assert!(!run.pack.exists());
        assert!(!run.receipt.exists());
    }

    #[test]
    fn recovery_preflights_completed_state_size_before_mutation() {
        let run = ready(false);
        interrupt(&run, "after-intent");
        let before = fs::read(&run.state).unwrap();
        let state = read_state(&run.state).unwrap();
        let intent_limit = state_bytes(&state).unwrap().len() as u64 + 1;
        STATE_BYTE_LIMIT.with(|limit| limit.set(intent_limit));
        assert_eq!(step(&run.state, &[]).unwrap_err().code, "LOCAL_SIZE");
        assert_eq!(fs::read(&run.state).unwrap(), before);
        assert!(!run.pack.exists());
        assert!(!run.receipt.exists());
    }

    #[test]
    fn intent_hashes_and_request_identity_are_checked_before_recovery() {
        for field in [
            "pack_bytes",
            "pack_sha256",
            "receipt_bytes",
            "receipt_sha256",
            "request_sha256",
            "run_id",
        ] {
            let run = ready(false);
            interrupt(&run, "after-intent");
            let mut state: Value = serde_json::from_slice(&fs::read(&run.state).unwrap()).unwrap();
            state["activation"][field] = json!("substituted");
            fs::write(&run.state, serde_json::to_vec(&state).unwrap()).unwrap();
            assert_eq!(
                step(&run.state, &[]).unwrap_err().code,
                "MCP_STATE_TAMPERED",
                "{field}"
            );
            assert!(!run.pack.exists());
            assert!(!run.receipt.exists());
        }
    }

    #[test]
    fn receipt_request_and_run_binding_survive_payload_rehashing() {
        for field in ["request_sha256", "run_id"] {
            let run = ready(false);
            interrupt(&run, "after-intent");
            let mut state = read_state(&run.state).unwrap();
            let intent = state.activation.as_mut().unwrap();
            let mut receipt: Value = serde_json::from_str(&intent.receipt_bytes).unwrap();
            receipt[field] = json!("unrelated run");
            intent.receipt_bytes = frozen_json(&receipt).unwrap();
            intent.receipt_sha256 = sha256_hex(intent.receipt_bytes.as_bytes());
            fs::write(&run.state, state_bytes(&state).unwrap()).unwrap();
            assert_eq!(
                step(&run.state, &[]).unwrap_err().code,
                "MCP_STATE_TAMPERED"
            );
            assert!(!run.pack.exists());
            assert!(!run.receipt.exists());
        }
    }

    #[test]
    fn activation_only_retry_remains_available_after_budget_expiration() {
        let run = ready(false);
        let mut state = read_state(&run.state).unwrap();
        state.created_at = state.request.end.clone();
        let end = environment::parse_time(&state.request.end).unwrap();
        let mut answers = state.answers.values_mut().collect::<Vec<_>>();
        answers.sort_by_key(|answer| environment::parse_time(&answer.ingested_at).unwrap());
        for (index, answer) in answers.into_iter().enumerate() {
            answer.ingested_at = (end + time::Duration::seconds(index as i64 + 1))
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap();
        }
        fs::write(&run.state, state_bytes(&state).unwrap()).unwrap();
        let output = step(&run.state, &[]).unwrap();
        assert_eq!(output.status, "complete", "{:?}", output.diagnostics);
    }

    #[test]
    fn deadline_boundary_expires_exactly_at_budget() {
        let run = ready(false);
        let mut state = read_state(&run.state).unwrap();
        state.created_at = "2026-01-01T00:00:00Z".into();
        state.request.max_seconds = 2;
        assert!(elapsed(&state, "2026-01-01T00:00:01.999999999Z").is_ok());
        assert_eq!(
            elapsed(&state, "2026-01-01T00:00:02Z").unwrap_err().code,
            "MCP_DEADLINE"
        );
    }
}
