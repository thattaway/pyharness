//! The `agent_traces` unit battery: the header validation, the scripted upload's
//! request + cursor + trace-log contract, the retry/rate gates, the parent-chain +
//! git-context resolution, the preview, the find walk, and the outbox entry hash.

use super::*;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

/// One captured request (url, headers, body).
type CapturedRequest = (String, Vec<(String, String)>, String);

/// A scripted transport: one PUT slot at a time with its captured
/// request and scripted answer.
pub(super) struct ScriptedTraceHttp {
    requests: Mutex<Vec<CapturedRequest>>,
    answers: Mutex<VecDeque<Result<TraceHttpResponse, TraceHttpError>>>,
}

impl ScriptedTraceHttp {
    pub(super) fn new(answers: Vec<Result<TraceHttpResponse, TraceHttpError>>) -> Self {
        ScriptedTraceHttp {
            requests: Mutex::new(Vec::new()),
            answers: Mutex::new(answers.into_iter().collect()),
        }
    }

    pub(super) fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl TraceHttp for ScriptedTraceHttp {
    fn put<'a>(
        &'a self,
        url: &'a str,
        headers: Vec<(String, String)>,
        body: String,
        _timeout_ms: u64,
        _cancel: Option<&'a TraceUploadCancel>,
    ) -> Pin<Box<dyn Future<Output = Result<TraceHttpResponse, TraceHttpError>> + Send + 'a>> {
        let url = url.to_string();
        let answer = self.answers.lock().unwrap().pop_front();
        self.requests.lock().unwrap().push((url, headers, body));
        Box::pin(async move { answer.expect("a scripted answer for the request") })
    }
}

pub(super) fn response(status: u16, body: &str) -> TraceHttpResponse {
    TraceHttpResponse {
        status,
        body: body.to_string(),
        retry_after: None,
    }
}

pub(super) struct Fixture {
    /// The temp dir stays alive for the fixture's life (the paths
    /// point into it); it is never read.
    _dir: tempfile::TempDir,
    pub(super) cwd: PathBuf,
    pub(super) agent_dir: PathBuf,
    pub(super) session_dir: PathBuf,
}

impl Fixture {
    pub(super) fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let agent_dir = dir.path().join("agent");
        let session_dir = agent_dir.join("sessions");
        std::fs::create_dir_all(&session_dir).expect("dirs");
        Fixture {
            cwd: dir.path().to_path_buf(),
            agent_dir,
            session_dir,
            _dir: dir,
        }
    }

    pub(super) fn write_session(&self, name: &str, id: &str) -> PathBuf {
        let path = self.session_dir.join(name);
        std::fs::write(
            &path,
            format!(
                "{{\"type\":\"session\",\"id\":\"{id}\",\"timestamp\":\"2026-01-01T00:00:00.000Z\",\"cwd\":\"/w\",\"version\":3}}\n{{\"type\":\"message\",\"id\":\"m1\",\"parentId\":null,\"message\":{{\"role\":\"user\",\"content\":\"hi\",\"timestamp\":0}}}}\n"
            ),
        )
        .expect("session file");
        path
    }

    fn options<'a>(
        &'a self,
        http: &'a dyn TraceHttp,
        session_file: Option<&'a Path>,
    ) -> TraceUploadOptions<'a> {
        TraceUploadOptions {
            session_file,
            cwd: &self.cwd,
            agent_dir: &self.agent_dir,
            require_enabled: false,
            reload_config: false,
            base_url: None,
            http,
            request_timeout_ms: DEFAULT_REQUEST_TIMEOUT_MS,
            cancel: None,
            on_upload_delay: None,
        }
    }
}

/// The engine reads process env (the credential keys); the tests that
/// touch it serialize on one lock.
pub(super) fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Clears the credential env vars under the lock and restores the prior
/// values (or absence) on drop — including on assertion panics.
pub(super) struct TraceEnvCredentials {
    saved: [(&'static str, Option<std::ffi::OsString>); 2],
}

pub(super) fn clear_trace_credentials() -> TraceEnvCredentials {
    let saved = [
        (
            "PYHARNESS_TRACES_API_KEY",
            std::env::var_os("PYHARNESS_TRACES_API_KEY"),
        ),
        ("PRIME_API_KEY", std::env::var_os("PRIME_API_KEY")),
    ];
    std::env::remove_var("PYHARNESS_TRACES_API_KEY");
    std::env::remove_var("PRIME_API_KEY");
    TraceEnvCredentials { saved }
}

impl Drop for TraceEnvCredentials {
    fn drop(&mut self) {
        for (name, value) in &self.saved {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

#[test]
fn the_header_validation_matches_ts() {
    let fixture = Fixture::new();
    let good = fixture.write_session("good.jsonl", "s1");
    assert!(read_trace_session_header(&good).is_some());
    // A message first line is not a session header.
    let bad = fixture.session_dir.join("bad.jsonl");
    std::fs::write(
        &bad,
        "{\"type\":\"message\",\"id\":\"m\",\"timestamp\":\"t\",\"cwd\":\"/w\"}\n",
    )
    .expect("bad file");
    assert!(read_trace_session_header(&bad).is_none());
    // A blank first line has no header.
    let blank = fixture.session_dir.join("blank.jsonl");
    std::fs::write(&blank, "   \n").expect("blank file");
    assert!(read_trace_session_header(&blank).is_none());
}

/// Trace sharing is permanently off: a settings file that opts in, a stored
/// credential, and a manual upload (no consent requirement) still send nothing.
#[tokio::test]
#[allow(clippy::await_holding_lock)] // the env lock must span the upload that reads the env
async fn no_upload_ever_reaches_the_network() {
    let _env = env_lock();
    let _credentials = clear_trace_credentials();
    std::env::set_var("PYHARNESS_TRACES_API_KEY", "key");
    let fixture = Fixture::new();
    let session = fixture.write_session("s.jsonl", "sid");
    std::fs::write(
        fixture.agent_dir.join("settings.json"),
        r#"{"agentTraces":{"enabled":true}}"#,
    )
    .expect("an opted-in settings file");
    let http = ScriptedTraceHttp::new(vec![Ok(response(200, "{}"))]);
    let options = fixture.options(&http, Some(&session));
    assert_eq!(upload_trace_file(&options).await, TraceUploadResult::Disabled);
    assert_eq!(http.request_count(), 0);
}

#[tokio::test]
async fn the_parent_chain_resolves_the_trace_id() {
    let fixture = Fixture::new();
    let parent = fixture.write_session("parent.jsonl", "root-id");
    let child = fixture.session_dir.join("child.jsonl");
    std::fs::write(
        &child,
        format!(
            "{{\"type\":\"session\",\"id\":\"child-id\",\"timestamp\":\"2026-01-01T00:00:00.000Z\",\"cwd\":\"/w\",\"parentSession\":\"{}\",\"version\":3}}\n",
            parent.display()
        ),
    )
    .expect("child file");
    let header = read_trace_session_header(&child).expect("header");
    let (trace_id, parent_session_id) = resolve_trace_context(&child, &header);
    assert_eq!(trace_id, "root-id");
    assert_eq!(parent_session_id.as_deref(), Some("root-id"));
}

#[tokio::test]
async fn the_active_git_context_walks_leaf_to_root() {
    let fixture = Fixture::new();
    let session = fixture.session_dir.join("s.jsonl");
    std::fs::write(
        &session,
        concat!(
            "{\"type\":\"session\",\"id\":\"s\",\"timestamp\":\"t\",\"cwd\":\"/w\"}\n",
            "{\"type\":\"message\",\"id\":\"m1\",\"parentId\":null,\"message\":{\"role\":\"user\",\"content\":\"hi\",\"timestamp\":0}}\n",
            "{\"type\":\"git_state\",\"id\":\"g1\",\"parentId\":\"m1\",\"git\":{\"repoUrl\":\"https://example/repo\",\"commit\":\"abc\"}}\n",
            "{\"type\":\"message\",\"id\":\"m2\",\"parentId\":\"g1\",\"message\":{\"role\":\"user\",\"content\":\"go\",\"timestamp\":1}}\n",
        ),
    )
    .expect("session file");
    let body = std::fs::read_to_string(&session).expect("body");
    let header = read_trace_session_header(&session).expect("header");
    assert_eq!(
        active_git_context(&body, &header),
        (
            Some("https://example/repo".to_string()),
            Some("abc".to_string())
        )
    );
}

#[test]
fn the_content_preview_splits_head_and_tail() {
    let body = "0123456789abcdef".repeat(4);
    // The marker takes 33 chars of the window; the remaining 6 split
    // into the head and tail halves.
    let (content, truncated) = trace_content_preview(&body, 33 + 6);
    assert!(truncated);
    let parts: Vec<&str> = content.split("... middle of trace omitted ...").collect();
    assert_eq!(parts.len(), 2);
    // The marker's newlines stay with their sides after the split.
    assert_eq!(parts[0].trim_end(), "012");
    assert_eq!(parts[1].trim_start(), "def");
    let (whole, truncated) = trace_content_preview("short", 10);
    assert_eq!(whole, "short");
    assert!(!truncated);
}

#[tokio::test]
async fn the_preview_reports_the_ts_fields() {
    let fixture = Fixture::new();
    let session = fixture.write_session("s.jsonl", "sid-1");
    match preview_trace_file(Some(&session), Some("https://api.example/"), None).await {
        TracePreviewResult::Ready(data) => {
            let (session_id, trace_id, endpoint, uploadable, truncated) = (
                data.session_id,
                data.trace_id,
                data.endpoint,
                data.uploadable,
                data.truncated,
            );
            assert_eq!(session_id, "sid-1");
            assert_eq!(trace_id, "sid-1");
            assert_eq!(
                endpoint,
                "https://api.example/api/v1/agent-traces/sessions/sid-1"
            );
            assert!(uploadable);
            assert!(!truncated);
        }
        other => panic!("expected a ready preview, got {other:?}"),
    }
    assert_eq!(
        preview_trace_file(None, None, None).await,
        TracePreviewResult::NoSessionFile
    );
}

#[tokio::test]
async fn the_find_walks_both_roots_for_headered_jsonl() {
    let fixture = Fixture::new();
    let session = fixture.write_session("s.jsonl", "sid");
    std::fs::create_dir_all(session_artifacts_root(&fixture.session_dir)).expect("artifacts dir");
    std::fs::write(
        session_artifacts_root(&fixture.session_dir).join("notes.txt"),
        "not a session",
    )
    .expect("notes");
    let files = find_trace_files(&fixture.session_dir);
    assert_eq!(files, vec![session]);
}

#[test]
fn the_retry_after_parsing_covers_seconds_and_dates() {
    assert_eq!(retry_after_delay(Some("2"), MAX_TIMER_DELAY_MS), Some(2000));
    assert_eq!(retry_after_delay(Some(""), MAX_TIMER_DELAY_MS), None);
    assert_eq!(retry_after_delay(None, MAX_TIMER_DELAY_MS), None);
    let past = format_http_date(1000);
    assert_eq!(retry_after_delay(Some(&past), MAX_TIMER_DELAY_MS), Some(0));
    let future_ms = now_ms() + 5000;
    let future = format_http_date(future_ms);
    let parsed = retry_after_delay(Some(&future), MAX_TIMER_DELAY_MS).expect("future");
    assert!((4000..=5000).contains(&parsed), "{parsed}");
}

fn format_http_date(ms: u64) -> String {
    let days = ms / 86_400_000;
    let time_ms = ms % 86_400_000;
    let (hour, minute, second) = (
        time_ms / 3_600_000,
        (time_ms % 3_600_000) / 60_000,
        (time_ms % 60_000) / 1000,
    );
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let names = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let weekday = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][((days + 4) % 7) as usize];
    format!(
        "{weekday}, {d:02} {} {y} {hour:02}:{minute:02}:{second:02} GMT",
        names[(m - 1) as usize]
    )
}

#[tokio::test(start_paused = true)]
// The env lock is held across awaits by design (env must stay stable).
#[allow(clippy::await_holding_lock)]
async fn the_rate_gate_reports_the_wait_and_serializes() {
    let _env = env_lock();
    let _credentials = clear_trace_credentials();
    std::env::set_var("PYHARNESS_TRACES_API_KEY", "trace-key");
    let fixture = Fixture::new();
    let http = ScriptedTraceHttp::new(vec![]);
    let gate = TraceRequestGate::new();
    let delays = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let observed = delays.clone();
    let options = TraceUploadOptions {
        session_file: None,
        cwd: &fixture.cwd,
        agent_dir: &fixture.agent_dir,
        require_enabled: false,
        reload_config: false,
        base_url: None,
        http: &http,
        request_timeout_ms: DEFAULT_REQUEST_TIMEOUT_MS,
        cancel: None,
        on_upload_delay: Some(Arc::new(move |delay| {
            if let TraceUploadDelay::RateLimit(ms) = delay {
                observed.fetch_max(ms, Ordering::SeqCst);
            }
        })),
    };
    // The first slot arms the interval; the second waits it out.
    gate.before_request(options.cancel, options.on_upload_delay.as_ref())
        .await
        .expect("the first slot");
    gate.before_request(None, options.on_upload_delay.as_ref())
        .await
        .expect("the second slot");
    assert!(delays.load(Ordering::SeqCst) >= TRACE_UPLOAD_ALL_MIN_REQUEST_INTERVAL_MS - 1000);
}

#[tokio::test]
// The env lock is held across awaits by design (env must stay stable).
#[allow(clippy::await_holding_lock)]
async fn the_cancel_stops_the_sweep_between_files() {
    let _env = env_lock();
    let _credentials = clear_trace_credentials();
    std::env::set_var("PYHARNESS_TRACES_API_KEY", "trace-key");
    let fixture = Fixture::new();
    fixture.write_session("a.jsonl", "sid-a");
    fixture.write_session("b.jsonl", "sid-b");
    let http = ScriptedTraceHttp::new(vec![]);
    let cancel = TraceUploadCancel::new();
    cancel.cancel();
    let options = TraceUploadAllOptions {
        session_dir: Some(&fixture.session_dir),
        cwd: &fixture.cwd,
        agent_dir: &fixture.agent_dir,
        require_enabled: false,
        reload_config: false,
        base_url: None,
        http: &http,
        request_timeout_ms: DEFAULT_REQUEST_TIMEOUT_MS,
        cancel: Some(&cancel),
        on_upload_delay: None,
        concurrency: Some(2),
        progress: None,
    };
    let result = upload_all_traces(&options).await;
    assert_eq!(result.total, 2);
    assert_eq!(result.uploaded, 0);
    assert_eq!(result.skipped, 2);
}

#[test]
fn the_uri_component_encoding_matches_javascript() {
    assert_eq!(encode_uri_component("abc-123_._~*'()!"), "abc-123_._~*'()!");
    assert_eq!(encode_uri_component("a/b"), "a%2Fb");
    assert_eq!(encode_uri_component("sp ace"), "sp%20ace");
    assert_eq!(encode_uri_component("ü"), "%C3%BC");
}

#[test]
fn the_outbox_entry_path_is_the_path_hash() {
    let fixture = Fixture::new();
    let session = fixture.session_dir.join("s.jsonl");
    let path = agent_trace_outbox_entry_path(&fixture.agent_dir, &session);
    assert_eq!(path.extension().and_then(|ext| ext.to_str()), Some("json"));
    assert_eq!(path.file_stem().unwrap().len(), 32);
    assert_eq!(
        agent_trace_outbox_entry_path(&fixture.agent_dir, &session),
        path
    );
    assert_ne!(
        agent_trace_outbox_entry_path(&fixture.agent_dir, &fixture.session_dir.join("t.jsonl")),
        path
    );
}

