use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

const SERVER: &str = "synthetic-server";

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "cqlmatic-capture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self(path)
    }
    fn state(&self) -> PathBuf {
        self.0.join("workflow/state.json")
    }
    fn saved(&self) -> Value {
        serde_json::from_slice(&fs::read(self.state()).unwrap()).unwrap()
    }
    fn tally(&self) -> Value {
        serde_json::from_slice(
            &fs::read(self.0.join("workflow/state.json.captures/tally.json")).unwrap(),
        )
        .unwrap()
    }
    /// Confirms a selected-source target in view `third-party` with source `#type=synthetic`.
    fn confirm(&self) {
        self.act_on(
            &self.state(),
            json!({"kind":"confirm-target","environment":environment(),"scope":"selected-sources","context":"Synthetic capture target","expected_sources":[{"type":"synthetic"}]}),
        );
    }
    fn act_on(&self, state: &Path, action: Value) {
        let request = self.0.join("request.json");
        fs::write(
            &request,
            json!({"schema_version":1,"action":action}).to_string(),
        )
        .unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
            .args(["env", "inventory", "--json", "--state"])
            .arg(state)
            .arg("--request")
            .arg(request)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    /// Runs the hook command on one payload; it must stay silent and succeed whatever happens.
    fn capture(&self, payload: Value) {
        self.capture_as("claude-code", SERVER, payload);
    }
    fn capture_as(&self, client: &str, server: &str, payload: Value) {
        self.capture_into(&self.state(), client, server, payload);
    }
    fn capture_into(&self, state: &Path, client: &str, server: &str, payload: Value) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
            .args(["env", "capture", "--client", client, "--server"])
            .arg(server)
            .arg("--state")
            .arg(state)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(0), "{payload}");
        assert!(
            output.stdout.is_empty() && output.stderr.is_empty(),
            "{output:?}"
        );
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn environment() -> Value {
    json!({"cloud":"us1","tenant":"00000000000000000000000000000000","view":"third-party"})
}

fn hours_ago(hours: i64) -> String {
    (time::OffsetDateTime::now_utc() - time::Duration::hours(hours))
        .replace_nanosecond(0)
        .unwrap()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap()
}

/// A search through the hosted server's gateway tool, as Claude Code reports it after the call.
fn search(query: &str, response: Value) -> Value {
    json!({
        "hook_event_name": "PostToolUse",
        "tool_name": format!("mcp__{SERVER}__execute_tool"),
        "tool_input": {"name": "falcon_search_ngsiem", "arguments": {
            "repository": "third-party", "query_string": query,
            "start": hours_ago(2), "end": hours_ago(1)
        }},
        "tool_response": response,
    })
}

fn body(rows: Value) -> String {
    json!({"result": rows, "trace_id": "00000000-0000-0000-0000-000000000000"}).to_string()
}

#[test]
fn capture_credits_a_plain_search_in_every_claude_code_result_form() {
    let dir = Temp::new();
    dir.confirm();
    let query = "#type=synthetic | head(5)";

    dir.capture(search(
        query,
        json!(body(json!([{"user": "ann", "bytes": "10"}]))),
    ));
    let mut listed = search(
        query,
        json!([{"type": "text", "text": body(json!([{"user": "bob", "empty": ""}]))}]),
    );
    // A direct search tool, and a call without `end`, which searched up to the moment it ran.
    listed["tool_name"] = json!(format!("mcp__{SERVER}__falcon_search_ngsiem"));
    listed["tool_input"] = listed["tool_input"]["arguments"].take();
    listed["tool_input"].as_object_mut().unwrap().remove("end");
    dir.capture(listed);

    let results = dir.0.join("session/tool-results");
    fs::create_dir_all(&results).unwrap();
    let saved = results.join("mcp-synthetic-server-execute_tool-1-a.txt");
    fs::write(&saved, body(json!([{"host": "synthetic-host"}]))).unwrap();
    let notice = |path: &Path| {
        let mut payload = search(
            query,
            json!(format!(
                "Error: result (91,246 characters) exceeds maximum allowed tokens. Output has been saved to {}.\nFormat: JSON with schema: {{result: [{{...}}], trace_id: string}}\n",
                path.display()
            )),
        );
        payload["transcript_path"] = json!(dir.0.join("session.jsonl"));
        payload
    };
    dir.capture(notice(&saved));
    // A notice may only name a file among the session's own saved results.
    let outside = dir.0.join("outside.txt");
    fs::write(&outside, body(json!([{"stray": "x"}]))).unwrap();
    dir.capture(notice(&outside));

    let state = dir.saved();
    let observations = state["captured_observations"].as_array().unwrap();
    let fields = |index: usize| {
        observations[index]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|field| field["field"].as_str().unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(observations.len(), 3, "{state}");
    // Captures are kept apart from imported observations.
    assert_eq!(state["query_observations"], json!([]));
    assert_eq!(fields(0), ["bytes", "user"]);
    assert_eq!(fields(1), ["user"]);
    assert_eq!(fields(2), ["host"]);
    for observation in observations {
        assert_eq!(observation["caller_assertion"], "client-hook-capture");
        assert_eq!(observation["environment"]["view"], "third-party");
    }
    assert_eq!(dir.tally(), json!({"recorded": 3, "search-failed": 1}));

    // Each credited search keeps a private copy of its query and result.
    let captures = dir.0.join("workflow/state.json.captures");
    let kept = |extension: &str| {
        fs::read_dir(&captures)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|found| found == extension))
            .collect::<Vec<_>>()
    };
    assert_eq!(kept("cql").len(), 3);
    assert_eq!(kept("json").len(), 4, "three results and the tally");
    assert!(
        kept("cql")
            .iter()
            .all(|path| fs::read_to_string(path).unwrap() == query)
    );
    #[cfg(unix)]
    for path in kept("cql").into_iter().chain(kept("json")) {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o077,
            0,
            "{path:?}"
        );
    }
}

#[test]
fn capture_flags_an_unmapped_source_and_counts_what_it_skips() {
    let dir = Temp::new();
    // Without a confirmed inventory there is nothing to add to, and nothing is created.
    dir.capture(search("#type=synthetic", json!(body(json!([{"a": "1"}])))));
    assert!(!dir.0.join("workflow").exists());
    dir.confirm();
    let rows = || json!(body(json!([{"user": "ann"}])));

    let unmapped = "#type=unmapped user=ann | head(5)";
    dir.capture(search(unmapped, rows()));
    dir.capture(search(unmapped, rows()));
    let state = dir.saved();
    let pending = state["pending_inventory"].as_array().unwrap();
    assert_eq!(pending.len(), 1, "{state}");
    assert_eq!(pending[0]["source"], json!({"type": "unmapped"}));
    assert_eq!(pending[0]["new_source"], true);
    assert_eq!(pending[0]["fields"], json!(["user"]));

    // Mapped source, but an aggregate proves nothing about its fields.
    dir.capture(search("#type=synthetic | count()", rows()));
    dir.capture(search("user=ann | head(5)", rows()));
    dir.capture(search("#type=synthetic", json!(body(json!([])))));
    dir.capture(search(
        "#type=synthetic",
        json!(json!({"result": {"error": "Failed to start NGSIEM search"}}).to_string()),
    ));
    let mut failed = search("#type=synthetic", Value::Null);
    failed["hook_event_name"] = json!("PostToolUseFailure");
    failed.as_object_mut().unwrap().remove("tool_response");
    failed["error"] = json!("Search failed");
    dir.capture(failed);
    let mut elsewhere = search("#type=synthetic", rows());
    elsewhere["tool_input"]["arguments"]["repository"] = json!("search-all");
    dir.capture(elsewhere);

    // Other servers and other tools are not searches of this inventory's target.
    let mut other_server = search("#type=synthetic", rows());
    other_server["tool_name"] = json!("mcp__another-server__execute_tool");
    dir.capture(other_server);
    let mut other_tool = search("#type=synthetic", rows());
    other_tool["tool_input"]["name"] = json!("another_tool");
    dir.capture(other_tool);
    dir.capture(json!("not a hook payload"));

    assert_eq!(
        dir.tally(),
        json!({
            "new-source-lead": 1, "nothing-new": 1, "not-creditable": 1, "no-single-source": 1,
            "no-rows": 1, "search-failed": 2, "other-view": 1
        })
    );
    assert_eq!(dir.saved()["query_observations"], json!([]));
    assert!(dir.saved()["captured_observations"].is_null());
    let leftovers = fs::read_dir(dir.0.join("workflow/state.json.captures"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| !name.contains("tally.json"))
        .collect::<Vec<_>>();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn capture_reads_codex_and_generic_payloads() {
    let dir = Temp::new();
    dir.confirm();
    let query = "#type=synthetic | head(5)";
    // Codex hands over the MCP result object and spells the server name with underscores.
    let codex = |response: Value| {
        let mut payload = search(query, response);
        payload["tool_name"] = json!("mcp__synthetic_server__execute_tool");
        payload
    };
    let text = body(json!([{"user": "ann"}]));
    dir.capture_as(
        "codex",
        "synthetic_server",
        codex(json!({
            "content": [{"type": "text", "text": text}],
            "structuredContent": serde_json::from_str::<Value>(&text).unwrap()
        })),
    );
    // A structured copy that disagrees with the text is refused, as it is for an import.
    dir.capture_as(
        "codex",
        "synthetic_server",
        codex(json!({
            "content": [{"type": "text", "text": text}],
            "structuredContent": {"result": {"error": "Search failed"}}
        })),
    );
    dir.capture_as(
        "codex",
        "synthetic_server",
        codex(json!({"content": [{"type": "text", "text": "Search failed"}], "isError": true})),
    );
    // Only Claude Code is known to move a result into a file, so no other client's notice is read.
    let saved = dir.0.join("session/tool-results/saved.txt");
    fs::create_dir_all(saved.parent().unwrap()).unwrap();
    fs::write(&saved, body(json!([{"host": "synthetic-host"}]))).unwrap();
    let mut notice = codex(json!(format!(
        "Error: result exceeds maximum allowed tokens. Output has been saved to {}.",
        saved.display()
    )));
    notice["transcript_path"] = json!(dir.0.join("session.jsonl"));
    dir.capture_as("codex", "synthetic_server", notice);

    // The generic form needs only the tool name, its input and its result.
    dir.capture_as(
        "generic",
        SERVER,
        json!({
            "tool_name": format!("mcp__{SERVER}__falcon_search_ngsiem"),
            "tool_input": {
                "repository": "third-party", "query_string": query, "start": hours_ago(3)
            },
            "tool_response": body(json!([{"bytes": "10"}])),
        }),
    );

    let state = dir.saved();
    let fields = state["captured_observations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|observation| observation["fields"][0]["field"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(fields, ["user", "bytes"], "{state}");
    assert_eq!(dir.tally(), json!({"recorded": 2, "search-failed": 3}));
}

#[test]
fn capture_reads_copilot_cli_and_cursor_payloads() {
    let dir = Temp::new();
    dir.confirm();
    let query = "#type=synthetic | head(5)";
    let arguments = || {
        json!({
            "repository": "third-party", "query_string": query,
            "start": hours_ago(2), "end": hours_ago(1)
        })
    };

    // Copilot CLI joins server and tool with a hyphen and hands the result over as text.
    let copilot = |result: Value| {
        json!({
            "toolName": format!("{SERVER}-falcon_search_ngsiem"),
            "toolArgs": arguments(),
            "toolResult": result,
        })
    };
    let success = |text: String| json!({"resultType": "success", "textResultForLlm": text});
    dir.capture_as(
        "copilot-cli",
        SERVER,
        copilot(success(body(json!([{"user": "ann"}])))),
    );
    // A large result is named in a notice and read only from the temporary directory.
    let saved = std::env::temp_dir().join(format!(
        "{}-copilot-tool-output-synthetic.txt",
        std::process::id()
    ));
    fs::write(&saved, body(json!([{"host": "synthetic-host"}]))).unwrap();
    let notice = |path: &Path| {
        success(format!(
            "Output too large to read at once (92.9 KB). Saved to: {}\nConsider using tools like rg.\n\nPreview (first 500 chars):\n{{\"result\":[",
            path.display()
        ))
    };
    dir.capture_as("copilot-cli", SERVER, copilot(notice(&saved)));
    fs::remove_file(&saved).unwrap();
    let elsewhere = dir.0.join("1-copilot-tool-output-synthetic.txt");
    fs::write(&elsewhere, body(json!([{"stray": "x"}]))).unwrap();
    dir.capture_as("copilot-cli", SERVER, copilot(notice(&elsewhere)));
    let mut failed = copilot(Value::Null);
    failed.as_object_mut().unwrap().remove("toolResult");
    failed["error"] = json!("Search failed");
    dir.capture_as("copilot-cli", SERVER, failed);

    // Cursor names the server separately and hands arguments and result over as JSON text.
    let cursor = |server: &str, result: Value| {
        json!({
            "hook_event_name": "afterMCPExecution",
            "tool_name": "execute_tool",
            "mcp_server_name": server,
            "tool_input": json!({"name": "falcon_search_ngsiem", "arguments": arguments()})
                .to_string(),
            "result_json": result.to_string(),
        })
    };
    dir.capture_as(
        "cursor",
        SERVER,
        cursor(
            SERVER,
            json!({"content": [{"type": "text", "text": body(json!([{"bytes": "10"}]))}]}),
        ),
    );
    dir.capture_as(
        "cursor",
        SERVER,
        cursor(SERVER, json!({"result": [{"zone": "synthetic-zone"}]})),
    );
    dir.capture_as(
        "cursor",
        SERVER,
        cursor("another-server", json!({"result": [{"stray": "x"}]})),
    );

    let state = dir.saved();
    let fields = state["captured_observations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|observation| observation["fields"][0]["field"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(fields, ["user", "host", "bytes", "zone"], "{state}");
    assert_eq!(dir.tally(), json!({"recorded": 4, "search-failed": 2}));
}

#[test]
fn capture_reads_cline_gemini_kiro_and_qwen_payloads() {
    let dir = Temp::new();
    dir.confirm();
    let query = "#type=synthetic | head(5)";
    let arguments = || {
        json!({
            "repository": "third-party", "query_string": query,
            "start": hours_ago(2), "end": hours_ago(1)
        })
    };
    let result =
        |field: &str| json!({"content": [{"type": "text", "text": body(json!([{field: "x"}]))}]});

    // Cline: the command-line client's record, then the editor extension's text-only form.
    dir.capture_as(
        "cline",
        SERVER,
        json!({"hookName": "tool_result", "tool_result": {
            "name": format!("{SERVER}__falcon_search_ngsiem"),
            "input": arguments(), "output": result("a"), "error": null
        }}),
    );
    dir.capture_as(
        "cline",
        SERVER,
        json!({"postToolUse": {
            "toolName": format!("{SERVER}__falcon_search_ngsiem"),
            "parameters": arguments(), "result": result("b").to_string(), "success": true
        }}),
    );
    dir.capture_as(
        "cline",
        SERVER,
        json!({"tool_result": {
            "name": format!("{SERVER}__falcon_search_ngsiem"),
            "input": arguments(), "output": null, "error": "Search failed"
        }}),
    );

    // Gemini CLI names the server in `mcp_context` and wraps the text it gives the model.
    let gemini = |server: &str, response: Value| {
        json!({
            "hook_event_name": "AfterTool",
            "tool_name": "mcp_synthetic-server_falcon_search_ngsiem",
            "tool_input": arguments(),
            "tool_response": response,
            "mcp_context": {"server_name": server, "tool_name": "falcon_search_ngsiem"},
        })
    };
    let display = body(json!([{"c": "x"}]));
    let wrapped = format!("<untrusted_context>\n{display}\n</untrusted_context>");
    dir.capture_as(
        "gemini-cli",
        SERVER,
        gemini(
            SERVER,
            json!({"llmContent": [{"text": wrapped}], "returnDisplay": display}),
        ),
    );
    dir.capture_as(
        "gemini-cli",
        SERVER,
        gemini(
            SERVER,
            json!({
                "llmContent": "MCP tool reported tool error",
                "returnDisplay": "Error: MCP tool reported an error.",
                "error": {"message": "Search failed", "type": "mcp_tool_error"}
            }),
        ),
    );
    dir.capture_as(
        "gemini-cli",
        SERVER,
        gemini(
            "another-server",
            json!({"returnDisplay": body(json!([{"stray": "x"}]))}),
        ),
    );

    // Kiro reports `mcp_SERVER_TOOL`; its predecessor reported `@SERVER/TOOL`.
    for (name, field) in [
        (format!("mcp_{SERVER}_falcon_search_ngsiem"), "d"),
        (format!("@{SERVER}/falcon_search_ngsiem"), "e"),
    ] {
        dir.capture_as(
            "kiro",
            SERVER,
            json!({
                "hook_event_name": "PostToolUse",
                "tool_name": name,
                "tool_input": arguments(),
                "tool_response": {"success": true, "result": [result(field)]},
            }),
        );
    }
    dir.capture_as(
        "kiro",
        SERVER,
        json!({
            "tool_name": format!("mcp_{SERVER}_falcon_search_ngsiem"),
            "tool_input": arguments(),
            "tool_response": {"success": false, "error": ["Search failed"]},
        }),
    );

    dir.capture_as(
        "qwen-code",
        SERVER,
        json!({
            "hook_event_name": "PostToolUse",
            "tool_name": format!("mcp__{SERVER}__falcon_search_ngsiem"),
            "tool_input": arguments(),
            "tool_response": {
                "llmContent": [{"text": body(json!([{"f": "x"}]))}],
                "returnDisplay": body(json!([{"f": "x"}]))
            },
        }),
    );

    // The Copilot surfaces share one hook format.
    dir.capture_as(
        "copilot-vscode",
        SERVER,
        json!({
            "toolName": format!("{SERVER}-falcon_search_ngsiem"),
            "toolArgs": arguments(),
            "toolResult": {"resultType": "success", "textResultForLlm": body(json!([{"g": "x"}]))},
        }),
    );

    let state = dir.saved();
    let fields = state["captured_observations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|observation| observation["fields"][0]["field"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(fields, ["a", "b", "c", "d", "e", "f", "g"], "{state}");
    assert_eq!(dir.tally(), json!({"recorded": 7, "search-failed": 3}));
}

/// Makes a file look old enough for the tidy-up to consider it.
fn age(path: &Path) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
        .unwrap();
}

fn names(directory: &Path) -> Vec<String> {
    let mut names = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| !name.starts_with('.'))
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn capture_replaces_covered_captures_and_tidies_what_nothing_refers_to() {
    let dir = Temp::new();
    dir.confirm();
    let window = |start: i64, end: Option<i64>, rows: Value| {
        let mut payload = search("#type=synthetic | head(5)", json!(body(rows)));
        let arguments = &mut payload["tool_input"]["arguments"];
        arguments["start"] = json!(hours_ago(start));
        match end {
            Some(end) => arguments["end"] = json!(hours_ago(end)),
            None => drop(arguments.as_object_mut().unwrap().remove("end")),
        }
        payload
    };
    let captures = dir.0.join("workflow/state.json.captures");
    let audits = dir.0.join("workflow/query-observations");

    // A thirty-day search is credited, then replaced by a later-starting one showing more.
    dir.capture(window(24 * 30, Some(3), json!([{"a": "1", "b": "1"}])));
    let mut replaced = names(&captures);
    replaced.retain(|name| name != "tally.json");
    assert_eq!(replaced.len(), 2);
    let replaced_audit = names(&audits);
    // An import may reuse a capture's saved query with a result of its own. Its audit record
    // then names that query file, which has to outlive the capture.
    let reused = replaced
        .iter()
        .find(|name| name.ends_with(".cql"))
        .unwrap()
        .clone();
    let result = dir.0.join("workflow/imported-result.json");
    fs::write(&result, body(json!([{"z": "1"}]))).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&result, fs::Permissions::from_mode(0o600)).unwrap();
    }
    dir.act_on(
        &dir.state(),
        json!({
            "kind": "import-query-observations", "query_file": captures.join(&reused),
            "result_file": result, "environment": environment(), "source": {"type": "synthetic"},
            "start": "2026-01-01T00:00:00Z", "end": "2026-01-02T00:00:00Z",
            "retrieved_at": "2026-01-03T00:00:00Z",
            "caller_assertion": "exact-query-result-target-window"
        }),
    );
    dir.capture(window(2, Some(1), json!([{"a": "1", "b": "1", "c": "1"}])));
    // An earlier-starting search that shows nothing new writes nothing.
    dir.capture(window(5, Some(4), json!([{"a": "1"}])));
    let fields = |state: &Value| {
        state["captured_observations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|observation| observation["fields"].as_array().unwrap().len())
            .collect::<Vec<_>>()
    };
    assert_eq!(fields(&dir.saved()), [3]);
    assert_eq!(dir.tally(), json!({"recorded": 2, "already-covered": 1}));

    // Nothing recent is removed: both captures' copies and audit records are still here.
    assert_eq!(names(&captures).len(), 5, "{:?}", names(&captures));
    assert_eq!(names(&audits).len(), 3);
    for directory in [&captures, &audits] {
        for name in names(directory) {
            age(&directory.join(name));
        }
    }
    let plant = |path: PathBuf, text: &str, old: bool| {
        fs::write(&path, text).unwrap();
        if old {
            age(&path);
        }
    };
    plant(
        captures.join("20260101T000000Z-0123456789ab.json"),
        "{}",
        true,
    );
    plant(
        captures.join("20260101T000000Z-0123456789ab.cql"),
        "x",
        true,
    );
    plant(
        captures.join("20260103T000000Z-0123456789ab.cql"),
        "x",
        true,
    );
    plant(
        captures.join("20260102T000000Z-0123456789ab.json"),
        "{}",
        false,
    );
    plant(captures.join("notes.json"), "{}", true);
    plant(
        audits.join("imported.json"),
        r#"{"observation":{"caller_assertion":"exact-query-result-target-window"}}"#,
        true,
    );

    dir.capture(window(1, None, json!([{"d": "1"}])));
    assert_eq!(fields(&dir.saved()), [3, 1]);
    let kept = names(&captures);
    // The replaced capture's result is gone; its query stays because the import names it.
    assert_eq!(
        replaced
            .iter()
            .filter(|name| kept.contains(name))
            .collect::<Vec<_>>(),
        [&reused],
        "{kept:?}"
    );
    assert_eq!(
        kept.iter()
            .filter(|name| name.contains("0123456789ab") || *name == "notes.json")
            .collect::<Vec<_>>(),
        ["20260102T000000Z-0123456789ab.json", "notes.json"]
    );
    // The held capture's aged copies stay, beside the new capture's and the tally.
    assert_eq!(kept.len(), 1 + 2 + 2 + 2 + 1, "{kept:?}");
    let kept = names(&audits);
    assert!(!kept.contains(&replaced_audit[0]), "{kept:?}");
    assert!(kept.contains(&"imported.json".to_owned()));
    assert_eq!(kept.len(), 4, "{kept:?}");
    assert!(!audits.join(format!(".{}.lock", replaced_audit[0])).exists());
}

#[test]
fn capture_never_tidies_the_files_of_another_state_in_its_directory() {
    let dir = Temp::new();
    dir.confirm();
    let second = dir.0.join("workflow/second.json");
    dir.act_on(
        &second,
        json!({"kind":"confirm-target","environment":environment(),"scope":"selected-sources","context":"Second synthetic target","expected_sources":[{"type":"synthetic"}]}),
    );
    let rows = |field: &str| {
        search(
            "#type=synthetic | head(5)",
            json!(body(json!([{field: "1"}]))),
        )
    };
    dir.capture_into(&second, "claude-code", SERVER, rows("a"));
    let copies = dir.0.join("workflow/second.json.captures");
    let audits = dir.0.join("workflow/query-observations");
    let before = (names(&copies), names(&audits));
    assert_eq!((before.0.len(), before.1.len()), (3, 1));
    for (directory, files) in [(&copies, &before.0), (&audits, &before.1)] {
        for name in files {
            age(&directory.join(name));
        }
    }

    // The first state shares the directory and the audit folder, and tidies after recording.
    dir.capture(rows("b"));
    assert_eq!(dir.tally(), json!({"recorded": 1}));
    assert_eq!(names(&copies), before.0);
    assert!(before.1.iter().all(|name| names(&audits).contains(name)));
    let second: Value = serde_json::from_slice(&fs::read(second).unwrap()).unwrap();
    assert_eq!(second["captured_observations"].as_array().unwrap().len(), 1);
}

#[test]
fn a_state_holding_captures_among_its_imports_still_loads() {
    let dir = Temp::new();
    dir.confirm();
    let rows = |field: &str| {
        search(
            "#type=synthetic | head(5)",
            json!(body(json!([{field: "1"}]))),
        )
    };
    dir.capture(rows("a"));
    // An earlier layout kept captures in the import list.
    let mut state = dir.saved();
    let captures = state["captured_observations"].take();
    state
        .as_object_mut()
        .unwrap()
        .remove("captured_observations");
    state["query_observations"] = captures;
    fs::write(dir.state(), state.to_string()).unwrap();

    dir.capture(rows("b"));
    assert_eq!(dir.tally(), json!({"recorded": 2}));
    let state = dir.saved();
    assert_eq!(state["query_observations"].as_array().unwrap().len(), 1);
    assert_eq!(state["captured_observations"].as_array().unwrap().len(), 1);
}
