//! The only network-capable boundary. No generic authenticated endpoint is public.
use crate::{
    contracts::{Cloud, Diagnostic, EnvironmentId},
    environment::{parse_time, validate_identity},
    sha256_hex, storage,
};
use reqwest::{
    Method, Url,
    blocking::{Client, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Read,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub static CANCELLED: AtomicBool = AtomicBool::new(false);
const MAX_RESPONSE: u64 = 8_388_608;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credentials {
    client_id: String,
    client_secret: String,
    #[serde(default)]
    member_cid: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
}
impl Credentials {
    pub fn load(path: Option<&Path>, identity: &EnvironmentId) -> Result<Self, Diagnostic> {
        let credentials = Self::read(path)?;
        credentials.validate_for(identity)?;
        Ok(credentials)
    }
    pub fn identity(
        path: Option<&Path>,
        cloud: Cloud,
        view: String,
    ) -> Result<EnvironmentId, Diagnostic> {
        let credentials = Self::read(path)?;
        let identity = EnvironmentId {
            cloud,
            tenant: credentials.account_binding(cloud),
            view,
        };
        credentials.validate_for(&identity)?;
        Ok(identity)
    }
    fn read(path: Option<&Path>) -> Result<Self, Diagnostic> {
        let credentials = if let Some(path) = path {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let meta = std::fs::symlink_metadata(path).map_err(|_| {
                    Diagnostic::error("AUTH_FILE", "Cannot inspect credential file.")
                })?;
                if meta.permissions().mode() & 0o077 != 0 {
                    return Err(Diagnostic::error(
                        "AUTH_PERMISSIONS",
                        "Credential file must have mode 0600 (or stricter).",
                    ));
                }
            }
            // Same code on every platform: on Unix the metadata check above already reports AUTH_FILE.
            let raw = storage::read_bytes(path, 16_384).map_err(|d| {
                if d.code == "LOCAL_IO" {
                    Diagnostic::error("AUTH_FILE", "Cannot read credential file.")
                } else {
                    d
                }
            })?;
            Self::from_bytes(&raw)?
        } else {
            Self{client_id:std::env::var("FALCON_CLIENT_ID").map_err(|_|Diagnostic::error("AUTH_MISSING","Set FALCON_CLIENT_ID/FALCON_CLIENT_SECRET or use a protected credential file."))?,client_secret:std::env::var("FALCON_CLIENT_SECRET").map_err(|_|Diagnostic::error("AUTH_MISSING","FALCON_CLIENT_SECRET is missing."))?,member_cid:std::env::var("FALCON_MEMBER_CID").ok(),base_url:std::env::var("FALCON_BASE_URL").ok()}
        };
        if credentials.client_id.trim().is_empty() || credentials.client_secret.trim().is_empty() {
            return Err(Diagnostic::error(
                "AUTH_MISSING",
                "Credentials cannot be empty.",
            ));
        }
        Ok(credentials)
    }
    fn validate_for(&self, identity: &EnvironmentId) -> Result<(), Diagnostic> {
        validate_identity(identity)?;
        if identity.tenant.starts_with("api-client:") {
            if self.account_binding(identity.cloud) != identity.tenant {
                return Err(Diagnostic::error(
                    "AUTH_ACCOUNT",
                    "Credentials do not match this API-client account fingerprint.",
                ));
            }
        } else if self
            .member_cid
            .as_ref()
            .is_some_and(|cid| cid != &identity.tenant)
        {
            return Err(Diagnostic::error(
                "AUTH_TENANT",
                "member_cid conflicts with the explicit environment tenant.",
            ));
        }
        if self
            .base_url
            .as_ref()
            .is_some_and(|s| cloud_for_url(s) != Some(identity.cloud))
        {
            return Err(Diagnostic::error(
                "AUTH_CLOUD",
                "Credential base_url conflicts with the explicit cloud or is not an approved HTTPS cloud endpoint.",
            ));
        }
        Ok(())
    }
    pub fn account_binding(&self, cloud: Cloud) -> String {
        let data = serde_json::to_vec(&(cloud, &self.client_id, &self.member_cid))
            .expect("credential metadata serializes");
        format!("api-client:{}", sha256_hex(&data))
    }
    fn from_bytes(raw: &[u8]) -> Result<Self, Diagnostic> {
        let text = std::str::from_utf8(raw)
            .map_err(|_| Diagnostic::error("AUTH_FILE", "Credential file must be UTF-8."))?;
        if text.trim_start().starts_with('{') {
            return serde_json::from_str(text).map_err(|_| {
                Diagnostic::error(
                    "AUTH_FILE",
                    "Credential JSON has invalid or unknown fields.",
                )
            });
        }
        let mut fields = std::collections::BTreeMap::new();
        let mut pending: Option<String> = None;
        for line in text
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty() && !s.starts_with('#'))
        {
            if let Some(key) = pending.take() {
                if fields
                    .insert(key, line.trim_matches(['\"', '\'']).to_string())
                    .is_some()
                {
                    return Err(Diagnostic::error(
                        "AUTH_FILE",
                        "Duplicate credential field.",
                    ));
                }
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .or_else(|| line.split_once(':'))
                .map(|(a, b)| (a, b.trim()))
                .unwrap_or((line, ""));
            let normalized = key
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>();
            let key = match normalized.as_str() {
                "clientid" | "falconclientid" => "client_id",
                "clientsecret" | "falconclientsecret" => "client_secret",
                "baseurl" | "falconbaseurl" => "base_url",
                "membercid" | "falconmembercid" => "member_cid",
                _ => {
                    return Err(Diagnostic::error(
                        "AUTH_FILE",
                        "Unrecognized credential label; use client_id, client_secret, base_url and optional member_cid.",
                    ));
                }
            };
            if value.is_empty() {
                pending = Some(key.into());
            } else if fields
                .insert(key.into(), value.trim_matches(['\"', '\'']).into())
                .is_some()
            {
                return Err(Diagnostic::error(
                    "AUTH_FILE",
                    "Duplicate credential field.",
                ));
            }
        }
        if pending.is_some() {
            return Err(Diagnostic::error(
                "AUTH_FILE",
                "A credential label is missing its value.",
            ));
        }
        let client_id = fields
            .remove("client_id")
            .ok_or_else(|| Diagnostic::error("AUTH_FILE", "client_id is missing."))?;
        let client_secret = fields
            .remove("client_secret")
            .ok_or_else(|| Diagnostic::error("AUTH_FILE", "client_secret is missing."))?;
        Ok(Self {
            client_id,
            client_secret,
            base_url: fields.remove("base_url"),
            member_cid: fields.remove("member_cid"),
        })
    }
}
pub fn cloud_for_url(s: &str) -> Option<Cloud> {
    let u = Url::parse(s).ok()?;
    if u.scheme() != "https"
        || !u.username().is_empty()
        || u.password().is_some()
        || u.port().is_some_and(|p| p != 443)
        || u.query().is_some()
        || u.fragment().is_some()
        || !matches!(u.path(), "" | "/")
    {
        return None;
    }
    match u.host_str()? {
        "api.crowdstrike.com" => Some(Cloud::Us1),
        "api.us-2.crowdstrike.com" => Some(Cloud::Us2),
        "api.eu-1.crowdstrike.com" => Some(Cloud::Eu1),
        _ => None,
    }
}
pub struct FalconClient {
    http: Client,
    base: Url,
    credentials: Credentials,
    token: Option<(String, Instant)>,
}
fn transport(stage: &str, error: reqwest::Error) -> Diagnostic {
    let reason = if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connection failed"
    } else {
        "HTTP transport failed"
    };
    Diagnostic::error(
        "HTTP_TRANSPORT",
        format!("{stage}: {reason}. Authentication payloads are withheld."),
    )
}
fn remaining(deadline: Instant) -> Result<Duration, Diagnostic> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .map(|d| d.min(Duration::from_secs(15)))
        .ok_or_else(|| Diagnostic::error("HTTP_DEADLINE", "Operation deadline expired."))
}
fn epoch_millis(s: &str) -> Result<i64, Diagnostic> {
    let date = parse_time(s)?;
    if date.unix_timestamp_nanos() % 1_000_000 != 0 {
        return Err(Diagnostic::error(
            "SEARCH_TIME_PRECISION",
            "Search windows support whole milliseconds; sub-millisecond boundaries are not rounded silently.",
        ));
    }
    i64::try_from(date.unix_timestamp_nanos() / 1_000_000).map_err(|_| {
        Diagnostic::error(
            "SEARCH_TIME_RANGE",
            "Timestamp exceeds the API millisecond range.",
        )
    })
}
fn retry_after(
    value: Option<&str>,
    now: time::OffsetDateTime,
) -> Result<Option<Duration>, Diagnostic> {
    let Some(value) = value else { return Ok(None) };
    if let Ok(seconds) = value.parse::<u64>() {
        return Ok(Some(Duration::from_secs(seconds)));
    }
    let format = time::macros::format_description!(
        "[weekday repr:short], [day] [month repr:short] [year] [hour]:[minute]:[second] GMT"
    );
    let date = time::PrimitiveDateTime::parse(value, format)
        .map_err(|_| {
            Diagnostic::error(
                "HTTP_RETRY_AFTER",
                "Unrecognized Retry-After value; no early retry was attempted.",
            )
        })?
        .assume_utc();
    Ok(Some((date - now).try_into().unwrap_or(Duration::ZERO)))
}
fn response_json(mut response: Response) -> Result<Value, Diagnostic> {
    if response.content_length().is_some_and(|n| n > MAX_RESPONSE) {
        return Err(Diagnostic::error(
            "HTTP_RESPONSE_LIMIT",
            "Response exceeds the 8 MiB budget.",
        ));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut response)
        .take(MAX_RESPONSE + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            Diagnostic::error("HTTP_RESPONSE", "Cannot read the complete HTTP response.")
        })?;
    if bytes.len() as u64 > MAX_RESPONSE {
        return Err(Diagnostic::error(
            "HTTP_RESPONSE_LIMIT",
            "Response exceeds the byte budget.",
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| Diagnostic::error("HTTP_RESPONSE", "Expected a complete JSON response."))
}
impl FalconClient {
    pub fn new(identity: &EnvironmentId, credentials: Credentials) -> Result<Self, Diagnostic> {
        credentials.validate_for(identity)?;
        let base = match identity.cloud {
            Cloud::Us1 => "https://api.crowdstrike.com/",
            Cloud::Us2 => "https://api.us-2.crowdstrike.com/",
            Cloud::Eu1 => "https://api.eu-1.crowdstrike.com/",
        };
        let http = Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("cqlmatic/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| {
                Diagnostic::error(
                    "HTTP_CONFIGURATION",
                    "Cannot initialize the verified TLS client.",
                )
            })?;
        Ok(Self {
            http,
            base: Url::parse(base).expect("fixed cloud URL"),
            credentials,
            token: None,
        })
    }
    fn token(&mut self, deadline: Instant) -> Result<String, Diagnostic> {
        if let Some((token, expires)) = &self.token
            && *expires > Instant::now() + Duration::from_secs(15)
        {
            return Ok(token.clone());
        }
        let url = self.base.join("oauth2/token").expect("fixed endpoint");
        let mut form = vec![
            ("client_id", self.credentials.client_id.as_str()),
            ("client_secret", self.credentials.client_secret.as_str()),
        ];
        if let Some(cid) = &self.credentials.member_cid {
            form.push(("member_cid", cid));
        }
        let response = self
            .http
            .post(url)
            .timeout(remaining(deadline)?)
            .form(&form)
            .send()
            .map_err(|e| transport("OAuth", e))?;
        if !response.status().is_success() {
            return Err(Diagnostic::error(
                "AUTH_REJECTED",
                format!(
                    "OAuth returned HTTP {}. Verify region, credentials, and scopes locally.",
                    response.status().as_u16()
                ),
            ));
        }
        let value = response_json(response)?;
        let token = value
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Diagnostic::error("AUTH_RESPONSE", "OAuth response did not contain a token.")
            })?
            .to_string();
        let ttl = value
            .get("expires_in")
            .and_then(Value::as_u64)
            .unwrap_or(60)
            .min(86_400);
        self.token = Some((token.clone(), Instant::now() + Duration::from_secs(ttl)));
        Ok(token)
    }
    fn endpoint(&self, parts: &[&str]) -> Url {
        let mut url = self.base.clone();
        {
            let mut path = url.path_segments_mut().expect("fixed hierarchical URL");
            path.clear();
            for p in parts {
                path.push(p);
            }
        }
        url
    }
    fn request(
        &mut self,
        method: Method,
        url: Url,
        body: Option<&Value>,
        deadline: Instant,
        retry_get: bool,
    ) -> Result<Value, Diagnostic> {
        let mut retry = 0;
        let mut reauthenticated = false;
        loop {
            let token = self.token(deadline)?;
            let mut request = self
                .http
                .request(method.clone(), url.clone())
                .timeout(remaining(deadline)?)
                .bearer_auth(token);
            if let Some(body) = body {
                request = request.json(body);
            }
            let response = request.send();
            match response {
                Err(e) => {
                    if retry_get && retry < 2 && (e.is_timeout() || e.is_connect()) {
                        retry += 1;
                        wait(Duration::from_millis(250 * retry), deadline)?;
                        continue;
                    }
                    return Err(transport(
                        if method == Method::POST {
                            "Start request failed; outcome may be ambiguous and was not retried"
                        } else {
                            "API request"
                        },
                        e,
                    ));
                }
                Ok(response) => {
                    let status = response.status();
                    if status.as_u16() == 401 && !reauthenticated {
                        self.token = None;
                        reauthenticated = true;
                        continue;
                    }
                    if retry_get
                        && retry < 2
                        && (status.as_u16() == 429 || status.is_server_error())
                    {
                        retry += 1;
                        let delay = retry_after(
                            response
                                .headers()
                                .get("Retry-After")
                                .and_then(|v| v.to_str().ok()),
                            time::OffsetDateTime::now_utc(),
                        )?
                        .unwrap_or(Duration::from_millis(250 * retry));
                        wait(delay, deadline)?;
                        continue;
                    }
                    if method == Method::DELETE && (status.is_success() || status.as_u16() == 404) {
                        return Ok(json!({"deleted":true}));
                    }
                    if !status.is_success() {
                        let detail = response_json(response)
                            .ok()
                            .map(|v| {
                                let mut messages = Vec::new();
                                if let Some(errors) = v.get("errors").and_then(Value::as_array) {
                                    for e in errors.iter().take(3) {
                                        if let Some(s) = e.get("message").and_then(Value::as_str) {
                                            messages.push(s.to_string());
                                        }
                                    }
                                }
                                if messages.is_empty()
                                    && let Some(s) = v.get("message").and_then(Value::as_str)
                                {
                                    messages.push(s.into());
                                }
                                let mut text = messages.join("; ");
                                for secret in [
                                    Some(self.credentials.client_id.as_str()),
                                    Some(self.credentials.client_secret.as_str()),
                                    self.token.as_ref().map(|t| t.0.as_str()),
                                ]
                                .into_iter()
                                .flatten()
                                {
                                    if !secret.is_empty() {
                                        text = text.replace(secret, "[redacted]");
                                    }
                                }
                                text.chars()
                                    .filter(|c| !c.is_control())
                                    .take(1024)
                                    .collect::<String>()
                            })
                            .unwrap_or_default();
                        return Err(Diagnostic::error(
                            "HTTP_STATUS",
                            format!("API returned HTTP {}. {}", status.as_u16(), detail),
                        ));
                    }
                    return response_json(response);
                }
            }
        }
    }
    pub fn list_parsers(
        &mut self,
        offset: u32,
        limit: u32,
        deadline: Instant,
    ) -> Result<Value, Diagnostic> {
        if limit == 0 || limit > 100 || offset > 9999 {
            return Err(Diagnostic::error(
                "PARSER_BUDGET",
                "Use limit 1..100 and offset at most 9999.",
            ));
        }
        let mut url = self.endpoint(&["ngsiem-content", "queries", "parsers", "v1"]);
        url.query_pairs_mut()
            .append_pair("limit", &limit.to_string())
            .append_pair("offset", &offset.to_string())
            .append_pair("repository", "parsers-repository");
        self.request(Method::GET, url, None, deadline, true)
    }
    pub fn parser(
        &mut self,
        id: &str,
        template: bool,
        deadline: Instant,
    ) -> Result<Value, Diagnostic> {
        if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
            return Err(Diagnostic::error(
                "PARSER_ID",
                "Supply a bounded parser ID.",
            ));
        }
        let mut url = self.endpoint(&[
            "ngsiem-content",
            "entities",
            if template {
                "parsers-template"
            } else {
                "parsers"
            },
            "v1",
        ]);
        url.query_pairs_mut()
            .append_pair("ids", id)
            .append_pair("repository", "parsers-repository");
        self.request(Method::GET, url, None, deadline, true)
    }
}
pub trait SearchBackend {
    fn start(
        &mut self,
        view: &str,
        query: &str,
        start: &str,
        end: &str,
        deadline: Instant,
    ) -> Result<String, Diagnostic>;
    fn poll(&mut self, view: &str, job: &str, deadline: Instant) -> Result<Value, Diagnostic>;
    fn stop(&mut self, view: &str, job: &str, deadline: Instant) -> Result<(), Diagnostic>;
    fn delegated_lifecycle(&self) -> bool {
        false
    }
    fn observed_at(&self, _job: &str) -> Option<String> {
        None
    }
}
impl SearchBackend for FalconClient {
    fn start(
        &mut self,
        view: &str,
        query: &str,
        start: &str,
        end: &str,
        deadline: Instant,
    ) -> Result<String, Diagnostic> {
        let url = self.endpoint(&["humio", "api", "v1", "repositories", view, "queryjobs"]);
        let start_ms = epoch_millis(start)?;
        let end_ms = epoch_millis(end)?;
        let body = json!({"queryString":query,"start":start_ms,"end":end_ms,"isLive":false,"allowEventSkipping":false,"useIngestTime":false,"timeZone":"UTC"});
        let value = self.request(Method::POST, url, Some(&body), deadline, false)?;
        value
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
            .map(str::to_string)
            .ok_or_else(|| {
                Diagnostic::error(
                    "HTTP_START_AMBIGUOUS",
                    "Start response did not contain a usable job ID; the start was not retried.",
                )
            })
    }
    fn poll(&mut self, view: &str, job: &str, deadline: Instant) -> Result<Value, Diagnostic> {
        let url = self.endpoint(&["humio", "api", "v1", "repositories", view, "queryjobs", job]);
        self.request(Method::GET, url, None, deadline, true)
    }
    fn stop(&mut self, view: &str, job: &str, deadline: Instant) -> Result<(), Diagnostic> {
        let url = self.endpoint(&["humio", "api", "v1", "repositories", view, "queryjobs", job]);
        self.request(Method::DELETE, url, None, deadline, false)
            .map(|_| ())
    }
}

fn wait(duration: Duration, deadline: Instant) -> Result<(), Diagnostic> {
    let end = Instant::now().checked_add(duration).ok_or_else(|| {
        Diagnostic::error("HTTP_DEADLINE", "Retry interval exceeds the deadline.")
    })?;
    if end > deadline {
        return Err(Diagnostic::error(
            "HTTP_DEADLINE",
            "Required polling or retry delay exceeds the deadline.",
        ));
    }
    while Instant::now() < end {
        if CANCELLED.load(Ordering::Relaxed) {
            return Err(Diagnostic::error("CANCELLED", "Operation cancelled."));
        }
        std::thread::sleep(
            end.saturating_duration_since(Instant::now())
                .min(Duration::from_millis(50)),
        );
    }
    Ok(())
}
#[derive(Debug, Clone)]
pub struct JobPolicy {
    pub timeout: Duration,
    pub poll_interval: Duration,
    pub max_polls: u32,
}
impl Default for JobPolicy {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(180),
            poll_interval: Duration::from_millis(500),
            max_polls: 360,
        }
    }
}
#[derive(Debug, Serialize, Deserialize)]
pub struct JobReceipt {
    pub query: String,
    pub start: String,
    pub end: String,
    pub job_id: Option<String>,
    pub completion: String,
    pub cleanup: String,
    pub polls: u32,
    pub events: Vec<Value>,
    pub metadata: Option<Value>,
    #[serde(default)]
    pub issues: BTreeMap<String, Value>,
    pub diagnostic: Option<Diagnostic>,
}
impl JobReceipt {
    pub fn cleanup_complete(&self) -> bool {
        match self.cleanup.as_str() {
            "confirmed" | "delegated" => self.job_id.is_some(),
            "not-started" => self.job_id.is_none() && self.completion != "complete",
            _ => false,
        }
    }
}

pub fn run_job(
    backend: &mut impl SearchBackend,
    view: &str,
    query: &str,
    start: &str,
    end: &str,
    policy: &JobPolicy,
) -> JobReceipt {
    let mut receipt = JobReceipt {
        query: query.into(),
        start: start.into(),
        end: end.into(),
        job_id: None,
        completion: "error".into(),
        cleanup: "not-started".into(),
        polls: 0,
        events: vec![],
        metadata: None,
        issues: BTreeMap::new(),
        diagnostic: None,
    };
    let action = (|| {
        let from = parse_time(start)?;
        let to = parse_time(end)?;
        if !start.ends_with('Z') || !end.ends_with('Z') || from >= to {
            return Err(Diagnostic::error(
                "SEARCH_WINDOW",
                "Use increasing absolute UTC timestamps ending in Z.",
            ));
        }
        if policy.timeout.is_zero()
            || policy.timeout > Duration::from_secs(600)
            || policy.max_polls == 0
            || policy.max_polls > 1200
        {
            return Err(Diagnostic::error(
                "SEARCH_BUDGET",
                "Job timeout must be 1..600 seconds and polls 1..1200.",
            ));
        }
        if CANCELLED.load(Ordering::Relaxed) {
            return Err(Diagnostic::error(
                "CANCELLED",
                "Operation cancelled before start.",
            ));
        }
        let deadline = Instant::now() + policy.timeout;
        let job = backend.start(view, query, start, end, deadline)?;
        receipt.job_id = Some(job.clone());
        receipt.cleanup = "pending".into();
        for _ in 0..policy.max_polls {
            remaining(deadline)?;
            if CANCELLED.load(Ordering::Relaxed) {
                return Err(Diagnostic::error("CANCELLED", "Operation cancelled."));
            }
            let snapshot = backend.poll(view, &job, deadline)?;
            receipt.polls += 1;
            // Retain the latest nonempty issues per origin without accumulating poll snapshots.
            for (key, value) in [
                ("warnings", snapshot.get("warnings")),
                ("errors", snapshot.get("errors")),
                ("metaData.warnings", snapshot.pointer("/metaData/warnings")),
                ("metaData.errors", snapshot.pointer("/metaData/errors")),
            ] {
                if nonempty(value) {
                    receipt
                        .issues
                        .insert(key.into(), value.expect("nonempty").clone());
                }
            }
            if let Some(meta) = snapshot.get("metaData").filter(|m| m.is_object()) {
                receipt.metadata = Some(meta.clone());
            }
            let done = snapshot
                .get("done")
                .and_then(Value::as_bool)
                .ok_or_else(|| {
                    Diagnostic::error(
                        "SEARCH_RESPONSE",
                        "Poll response must include a boolean done field.",
                    )
                })?;
            let cancelled = snapshot
                .get("cancelled")
                .and_then(Value::as_bool)
                .ok_or_else(|| {
                    Diagnostic::error(
                        "SEARCH_RESPONSE",
                        "Poll response must include a boolean cancelled field.",
                    )
                })?;
            for (prefix, value) in [
                ("", Some(&snapshot)),
                ("metaData.", snapshot.get("metaData")),
            ] {
                if let Some(value) = value {
                    for (key, flag) in flagged_values(value) {
                        if !done && key.ends_with("hasMoreEvents") {
                            continue;
                        }
                        receipt
                            .issues
                            .insert(format!("{prefix}{key}"), flag.clone());
                    }
                    if let Some(live) = value
                        .pointer("/filterQuery/isLive")
                        .filter(|value| truth(value))
                    {
                        receipt
                            .issues
                            .insert(format!("{prefix}filterQuery.isLive"), live.clone());
                    }
                }
            }
            if cancelled {
                return Err(Diagnostic::error(
                    "SEARCH_CANCELLED",
                    "Backend cancelled the job; results are inconclusive.",
                ));
            }
            if receipt.issues.contains_key("errors")
                || receipt.issues.contains_key("metaData.errors")
            {
                return Err(Diagnostic::error(
                    "SEARCH_ERRORS",
                    "Backend reported search errors; results are inconclusive.",
                ));
            }
            if done {
                snapshot
                    .get("metaData")
                    .filter(|m| m.is_object())
                    .ok_or_else(|| {
                        Diagnostic::error("SEARCH_RESPONSE", "Completion metadata is missing.")
                    })?;
                if !receipt.issues.is_empty() {
                    return Err(Diagnostic::error(
                        "SEARCH_INCOMPLETE",
                        "Warnings, truncation, skipping, or a live result prevent complete-evidence attestation.",
                    ));
                }
                let events = snapshot
                    .get("events")
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        Diagnostic::error(
                            "SEARCH_RESPONSE",
                            "Completed response has no events array.",
                        )
                    })?;
                if events.len() >= 10_000 || events.iter().any(|e| !e.is_object()) {
                    return Err(Diagnostic::error(
                        "SEARCH_ROW_LIMIT",
                        "Result shape or row count exceeds the evidence budget.",
                    ));
                }
                receipt.events = events.clone();
                receipt.completion = "complete".into();
                return Ok(());
            }
            wait(policy.poll_interval, deadline)?;
        }
        Err(Diagnostic::error(
            "SEARCH_DEADLINE",
            "Polling budget exhausted; results are inconclusive.",
        ))
    })();
    if let Err(d) = action {
        receipt.completion = if d.code.contains("DEADLINE") {
            "timeout"
        } else if d.code == "CANCELLED" {
            "cancelled"
        } else {
            "error"
        }
        .into();
        receipt.diagnostic = Some(d);
    }
    if let Some(job) = &receipt.job_id {
        receipt.cleanup = if backend.delegated_lifecycle() {
            "delegated"
        } else if backend
            .stop(view, job, Instant::now() + Duration::from_secs(5))
            .is_ok()
        {
            "confirmed"
        } else {
            "unconfirmed"
        }
        .into();
    } else if receipt
        .diagnostic
        .as_ref()
        .is_some_and(|d| matches!(d.code.as_str(), "HTTP_START_AMBIGUOUS" | "HTTP_TRANSPORT"))
    {
        receipt.cleanup = "unknown-job".into();
    }
    receipt
}
fn truth(v: &Value) -> bool {
    v == &Value::Bool(true) || v.as_str().is_some_and(|s| s.eq_ignore_ascii_case("true"))
}
pub(crate) fn incomplete_flag(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        // Floating flags may already have underflowed to zero during JSON parsing.
        Value::Number(n) => n.as_i64() != Some(0) && n.as_u64() != Some(0),
        Value::String(s) => !s.is_empty(),
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
    }
}
fn nonempty(v: Option<&Value>) -> bool {
    v.is_some_and(|v| match v {
        Value::Null => false,
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::String(s) => !s.is_empty(),
        Value::Bool(b) => *b,
        _ => true,
    })
}
const COMPLETENESS_FLAGS: [&str; 8] = [
    "hasMoreEvents",
    "truncated",
    "partial",
    "limitReached",
    "eventsSkipped",
    "skippedEvents",
    "timedOut",
    "warnings",
];
pub(crate) fn is_completeness_flag(key: &str) -> bool {
    COMPLETENESS_FLAGS.contains(&key)
}
fn flagged_values(v: &Value) -> impl Iterator<Item = (String, &Value)> {
    COMPLETENESS_FLAGS
        .into_iter()
        .flat_map(move |key| {
            [
                (key.to_string(), v.get(key)),
                (
                    format!("extraData.{key}"),
                    v.get("extraData").and_then(|extra| extra.get(key)),
                ),
            ]
        })
        .filter_map(|(key, value)| {
            value
                .filter(|value| incomplete_flag(value))
                .map(|value| (key, value))
        })
}
pub(crate) fn flagged(v: &Value) -> bool {
    flagged_values(v).next().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fake {
        snapshots: std::collections::VecDeque<Value>,
        starts: u32,
        stops: u32,
        start_error: bool,
    }
    impl SearchBackend for Fake {
        fn start(
            &mut self,
            _: &str,
            _: &str,
            _: &str,
            _: &str,
            _: Instant,
        ) -> Result<String, Diagnostic> {
            self.starts += 1;
            if self.start_error {
                Err(Diagnostic::error("HTTP_START_AMBIGUOUS", "lost response"))
            } else {
                Ok("job".into())
            }
        }
        fn poll(&mut self, _: &str, _: &str, _: Instant) -> Result<Value, Diagnostic> {
            self.snapshots
                .pop_front()
                .ok_or_else(|| Diagnostic::error("TEST", "no poll"))
        }
        fn stop(&mut self, _: &str, _: &str, _: Instant) -> Result<(), Diagnostic> {
            self.stops += 1;
            Ok(())
        }
    }
    fn run(f: &mut Fake) -> JobReceipt {
        run_job(
            f,
            "search-all",
            "count()",
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:01:00Z",
            &JobPolicy {
                timeout: Duration::from_secs(1),
                poll_interval: Duration::ZERO,
                max_polls: 4,
            },
        )
    }
    #[test]
    fn snapshots_are_not_summed_and_cleanup_is_bounded() {
        let mut f = Fake {
            snapshots: vec![
                json!({"done":false,"cancelled":false,"events":[{"n":1}]}),
                json!({"done":true,"cancelled":false,"metaData":{},"events":[{"n":2}]}),
            ]
            .into(),
            starts: 0,
            stops: 0,
            start_error: false,
        };
        let r = run(&mut f);
        assert_eq!(r.events, json!([{"n":2}]).as_array().unwrap().clone());
        assert_eq!(f.stops, 1);
        assert_eq!(r.completion, "complete");
        assert_eq!(r.cleanup, "confirmed");
        assert!(r.cleanup_complete());
        assert!(!f.delegated_lifecycle());
        let mut credentials =
            Credentials::from_bytes(b"client_id=fixture\nclient_secret=fixture").unwrap();
        credentials.base_url = None;
        let identity = EnvironmentId {
            cloud: Cloud::Us1,
            tenant: "0".repeat(32),
            view: "search-all".into(),
        };
        let client = FalconClient::new(&identity, credentials).unwrap();
        assert!(!client.delegated_lifecycle());
    }
    #[test]
    fn cancellation_warning_and_truncation_are_not_zero_counts() {
        for extra in [
            json!({"cancelled":true}),
            json!({"warnings":["memory limit"]}),
            json!({"metaData":{"extraData":{"hasMoreEvents":"true"}}}),
            json!({"metaData":{"extraData":{"eventsSkipped":3}}}),
            json!({"metaData":{"partial":"unknown"}}),
        ] {
            let mut v = json!({"done":true,"cancelled":false,"metaData":{},"events":[]});
            for (k, x) in extra.as_object().unwrap() {
                v[k] = x.clone();
            }
            let mut f = Fake {
                snapshots: vec![v].into(),
                starts: 0,
                stops: 0,
                start_error: false,
            };
            assert_ne!(run(&mut f).completion, "complete");
            assert_eq!(f.stops, 1);
        }
    }
    #[test]
    fn ambiguous_start_is_never_retried() {
        let mut f = Fake {
            snapshots: Default::default(),
            starts: 0,
            stops: 0,
            start_error: true,
        };
        let r = run(&mut f);
        assert_eq!(f.starts, 1);
        assert_eq!(f.stops, 0);
        assert_eq!(r.cleanup, "unknown-job");
        assert!(!r.cleanup_complete());
    }
    #[test]
    fn transient_warnings_and_poll_errors_retain_their_evidence() {
        for (snapshots, key, code) in [
            (
                vec![
                    json!({"done":false,"cancelled":false,"warnings":["memory"],"metaData":{}}),
                    json!({"done":true,"cancelled":false,"metaData":{},"events":[]}),
                ],
                "warnings",
                "SEARCH_INCOMPLETE",
            ),
            (
                vec![
                    json!({"done":false,"cancelled":false,"metaData":{"errors":[{"message":"bad argument"}]}}),
                ],
                "metaData.errors",
                "SEARCH_ERRORS",
            ),
        ] {
            let mut backend = Fake {
                snapshots: snapshots.into(),
                starts: 0,
                stops: 0,
                start_error: false,
            };
            let receipt = run(&mut backend);
            assert_eq!(receipt.completion, "error");
            assert_eq!(receipt.cleanup, "confirmed");
            assert_eq!(receipt.diagnostic.as_ref().unwrap().code, code);
            assert!(!receipt.issues[key].is_null());
            let mut old = serde_json::to_value(receipt).unwrap();
            old.as_object_mut().unwrap().remove("issues");
            assert!(
                serde_json::from_value::<JobReceipt>(old)
                    .unwrap()
                    .issues
                    .is_empty()
            );
        }
    }
    #[test]
    fn absolute_times_and_retry_after_are_not_relative_guesses() {
        assert_eq!(epoch_millis("1970-01-01T00:00:01.123Z").unwrap(), 1123);
        assert!(epoch_millis("2026-10-29T00:00:00.000001Z").is_err());
        let now = time::macros::datetime!(2026-10-29 00:00 UTC);
        assert_eq!(
            retry_after(Some("3"), now).unwrap(),
            Some(Duration::from_secs(3))
        );
        assert_eq!(
            retry_after(Some("Thu, 29 Oct 2026 00:00:07 GMT"), now).unwrap(),
            Some(Duration::from_secs(7))
        );
        assert_eq!(
            retry_after(Some("Wed, 28 Oct 2026 23:59:59 GMT"), now).unwrap(),
            Some(Duration::ZERO)
        );
        assert!(retry_after(Some("eventually"), now).is_err());
    }
    #[test]
    fn credentials_and_clouds_have_explicit_boundaries() {
        let credentials = Credentials::from_bytes(b"client_id\nexample\nclient_secret\nnot-a-secret\nbase_url\nhttps://api.us-2.crowdstrike.com\n").unwrap();
        assert_ne!(
            credentials.account_binding(Cloud::Us1),
            credentials.account_binding(Cloud::Us2)
        );
        assert!(Credentials::from_bytes(b"client_id=a\nclient_id=b\nclient_secret=c").is_err());
        assert_eq!(
            cloud_for_url("https://api.us-2.crowdstrike.com/"),
            Some(Cloud::Us2)
        );
        for url in [
            "http://api.crowdstrike.com",
            "https://api.crowdstrike.com.evil.example",
            "https://example@api.crowdstrike.com",
            "https://api.crowdstrike.com/path",
            "https://api.crowdstrike.com/?x=y",
        ] {
            assert_eq!(cloud_for_url(url), None);
        }
    }
}
