use super::*;
use crate::agent_traces::tests::{response, Fixture, ScriptedTraceHttp};
use std::io::Write;

async fn recover(
    cwd: PathBuf,
    agent_dir: PathBuf,
    permits: Arc<tokio::sync::Semaphore>,
    http: Arc<dyn TraceHttp>,
    base_url: Option<String>,
    cancel: TraceUploadCancel,
) -> bool {
    recover_with_backoff(cwd, agent_dir, permits, http, base_url, cancel, None)
        .await
        .0
}

fn controller(fixture: &Fixture, path: &Path, enabled: bool) -> Arc<ContinuousTraceUpload> {
    Arc::new(ContinuousTraceUpload {
        cwd: fixture.cwd.clone(),
        agent_dir: Arc::new(fixture.agent_dir.clone()),
        consent: Mutex::new((
            enabled,
            ConsentGeneration::read(&fixture.cwd, &fixture.agent_dir),
        )),
        pending: Mutex::new(Some((path.to_path_buf(), Schedule::default()))),
        wake: Arc::new(tokio::sync::Notify::new()),
        cancel: TraceUploadCancel::new(),
        started: AtomicBool::new(false),
    })
}

#[test]
fn debounce_throttle_and_writes_in_flight_coalesce() {
    let now = Instant::now();
    let mut s = Schedule::default();
    s.persist(now);
    s.persist(now + Duration::from_millis(500));
    assert_eq!(s.due, Some(now + Duration::from_millis(1500)));
    let generation = s.start(now + Duration::from_millis(1500));
    s.persist(now + Duration::from_secs(2));
    s.settle(
        now + Duration::from_secs(3),
        generation,
        &TraceUploadResult::Unchanged,
    );
    assert_eq!(s.due, Some(now + Duration::from_millis(61500)));
    let generation = s.start(s.due.unwrap());
    s.settle(
        now + Duration::from_secs(62),
        generation,
        &TraceUploadResult::Unchanged,
    );
    assert_eq!(s.due, None);
}

#[test]
fn rate_limit_reschedule_honors_retry_after_and_terminal_failure_stops() {
    let now = Instant::now();
    let mut s = Schedule::default();
    let generation = s.start(now);
    s.settle(
        now + Duration::from_secs(1),
        generation,
        &TraceUploadResult::Failed {
            status_code: Some(429),
            message: "synthetic".into(),
            retry_after_ms: Some(120_000),
        },
    );
    assert_eq!(s.due, Some(now + Duration::from_secs(121)));
    let generation = s.start(s.due.unwrap());
    s.settle(
        now + Duration::from_secs(122),
        generation,
        &TraceUploadResult::Failed {
            status_code: Some(403),
            message: "synthetic".into(),
            retry_after_ms: None,
        },
    );
    assert_eq!(s.due, None);
}

#[test]
fn consent_off_never_registers_or_schedules_retroactively() {
    let fixture = Fixture::new();
    let path = fixture.write_session("off.jsonl", "off");
    let c = controller(&fixture, &path, false);
    c.persisted(&path);
    *c.consent.lock().unwrap() = (
        true,
        ConsentGeneration::read(&fixture.cwd, &fixture.agent_dir),
    );
    assert_eq!(c.pending.lock().unwrap().as_ref().unwrap().1.due, None);
    assert!(!agent_trace_outbox_entry_path(&fixture.agent_dir, &path).exists());
}

#[tokio::test(start_paused = true)]
async fn external_consent_change_fails_closed_until_background_reload() {
    let fixture = Fixture::new();
    let path = fixture.write_session("changed.jsonl", "changed");
    let c = controller(&fixture, &path, true);
    std::fs::write(
        fixture.agent_dir.join("settings.json"),
        r#"{"agentTraces":{"enabled":false}}"#,
    )
    .unwrap();
    c.persisted(&path);
    // The durable intent survives the external change; only the worker's
    // consent re-read decides whether it is ever delivered.
    assert!(c.pending.lock().unwrap().as_ref().unwrap().1.due.is_some());
    assert!(agent_trace_outbox_entry_path(&fixture.agent_dir, &path).exists());
    let (requests, mut observed) = tokio::sync::mpsc::unbounded_channel();
    let sink = Arc::new(ObservedSink {
        requests,
        cancelled: Arc::new(tokio::sync::Notify::new()),
    });
    let task = tokio::spawn(run_controller(
        Arc::downgrade(&c),
        Arc::new(tokio::sync::Semaphore::new(1)),
        sink,
        Some("http://synthetic.invalid".into()),
    ));
    tokio::time::advance(MIN_INTERVAL * 2).await;
    tokio::task::yield_now().await;
    assert!(
        observed.try_recv().is_err(),
        "a disabled project must never deliver its retained intent"
    );
    drop(c);
    task.await.unwrap();
}

#[test]
fn marker_is_complete_and_preserves_a_success_cursor() {
    let fixture = Fixture::new();
    let path = fixture.write_session("marker.jsonl", "marker");
    mark_pending(&fixture.agent_dir, &path).unwrap();
    let raw =
        std::fs::read_to_string(agent_trace_outbox_entry_path(&fixture.agent_dir, &path)).unwrap();
    assert_eq!(
        parse_outbox_entry(&raw),
        Some((path.to_string_lossy().into_owned(), None))
    );
    let signature = TraceUploadSignature::of(&path).unwrap();
    record_agent_trace_outbox_upload(&fixture.agent_dir, &path, signature).unwrap();
    mark_pending(&fixture.agent_dir, &path).unwrap();
    assert_eq!(
        read_agent_trace_outbox_entry(&fixture.agent_dir, &path),
        Some(signature)
    );
}

#[test]
fn competing_delivery_is_nonblocking_and_crash_recoverable() {
    let fixture = Fixture::new();
    let path = fixture.write_session("lease.jsonl", "lease");
    let lease = delivery_lease(&fixture.agent_dir, &path).unwrap();
    assert!(delivery_lease(&fixture.agent_dir, &path).is_none());
    drop(lease);
    assert!(delivery_lease(&fixture.agent_dir, &path).is_some());
}

#[tokio::test(start_paused = true)]
async fn controller_drop_cancels_timer_without_waiting_for_delivery() {
    let fixture = Fixture::new();
    let path = fixture.write_session("drop.jsonl", "drop");
    let c = controller(&fixture, &path, true);
    c.persisted(&path);
    let cancel = c.cancel.clone();
    let http = Arc::new(ScriptedTraceHttp::new(vec![Ok(response(200, "{}"))]));
    let task = tokio::spawn(run_controller(
        Arc::downgrade(&c),
        Arc::new(tokio::sync::Semaphore::new(1)),
        http,
        None,
    ));
    drop(c);
    assert!(cancel.is_cancelled());
    task.await.unwrap();
    assert!(agent_trace_outbox_entry_path(&fixture.agent_dir, &path).is_file());
}

// Informational default CI measurement; calibrated host benchmarks own latency gates.
#[test]
fn measure_synchronous_pending_marker_cost() {
    let fixture = Fixture::new();
    let mut cold = Vec::new();
    let mut warm = Vec::new();
    for index in 0..1000 {
        let path = fixture.session_dir.join(format!("synthetic-{index}.jsonl"));
        let c = controller(&fixture, &path, true);
        let start = std::time::Instant::now();
        c.persisted(&path);
        cold.push(start.elapsed().as_nanos());
        let start = std::time::Instant::now();
        c.persisted(&path);
        warm.push(start.elapsed().as_nanos());
    }
    cold.sort_unstable();
    warm.sort_unstable();
    let report = format!(
        "{}\n",
        json!({"samples":1000,"cold_marker_ns":{"p50":cold[500],"p95":cold[950],"p99":cold[990]},"warm_marker_ns":{"p50":warm[500],"p95":warm[950],"p99":warm[990]},"fsync":false,"consent_metadata_checks_included":false,"cached_consent_check_included":true})
    );
    // Explicit stdout retains this informational measurement in hosted CI logs
    // even when the test harness captures println output for passing tests.
    std::io::stdout().write_all(report.as_bytes()).unwrap();
}

struct ObservedSink {
    requests: tokio::sync::mpsc::UnboundedSender<(
        String,
        tokio::sync::oneshot::Sender<TraceHttpResponse>,
    )>,
    cancelled: Arc<tokio::sync::Notify>,
}

impl TraceHttp for ObservedSink {
    fn put<'a>(
        &'a self,
        _url: &'a str,
        _headers: Vec<(String, String)>,
        body: String,
        _timeout_ms: u64,
        cancel: Option<&'a TraceUploadCancel>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<TraceHttpResponse, TraceHttpError>> + Send + 'a,
        >,
    > {
        Box::pin(async move {
            let (reply, result) = tokio::sync::oneshot::channel();
            self.requests.send((body, reply)).unwrap();
            tokio::select! {
                answer = result => Ok(answer.unwrap()),
                () = async { if let Some(cancel) = cancel { cancel.wait().await; } else { std::future::pending::<()>().await; } } => {
                    self.cancelled.notify_one();
                    Err(TraceHttpError::Cancelled)
                }
            }
        })
    }
}

#[tokio::test(start_paused = true)]
async fn opted_out_recovery_does_not_create_an_outbox() {
    let fixture = Fixture::new();
    let sink = Arc::new(ScriptedTraceHttp::new(vec![]));
    recover(
        fixture.cwd.clone(),
        fixture.agent_dir.clone(),
        Arc::new(tokio::sync::Semaphore::new(1)),
        sink,
        Some("http://synthetic.invalid".into()),
        TraceUploadCancel::new(),
    )
    .await;
    assert!(!agent_trace_outbox_dir(&fixture.agent_dir).exists());
}

#[test]
fn persist_racing_a_pruner_retains_a_complete_nonblocking_fallback_marker() {
    let fixture = Fixture::new();
    let path = fixture.write_session("prune-race.jsonl", "race");
    let entry = agent_trace_outbox_entry_path(&fixture.agent_dir, &path);
    std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
    std::fs::write(&entry, "{").unwrap();
    let pruner = outbox_mutation_lock(&entry).unwrap();
    pruner.lock().unwrap();
    mark_pending(&fixture.agent_dir, &path).unwrap();
    let fallback = entry.with_extension("pending.json");
    let raw = std::fs::read_to_string(&fallback).unwrap();
    assert_eq!(
        parse_outbox_entry(&raw),
        Some((path.to_string_lossy().into_owned(), None))
    );
    drop(pruner);
    prune_entry(&entry, "{", None);
    assert!(!entry.exists());
    assert!(fallback.exists());
    prune_entry(&fallback, &raw, None);
    assert!(fallback.exists());
}

#[test]
fn stale_prune_observations_never_remove_a_new_cursor_or_recreated_transcript() {
    let fixture = Fixture::new();
    let path = fixture.write_session("stale-prune.jsonl", "stale");
    mark_pending(&fixture.agent_dir, &path).unwrap();
    let entry = agent_trace_outbox_entry_path(&fixture.agent_dir, &path);
    let raw = std::fs::read_to_string(&entry).unwrap();
    let signature = TraceUploadSignature::of(&path).unwrap();
    record_agent_trace_outbox_upload(&fixture.agent_dir, &path, signature).unwrap();
    prune_entry(&entry, &raw, Some(&path));
    assert_eq!(
        read_agent_trace_outbox_entry(&fixture.agent_dir, &path),
        Some(signature)
    );
    let raw = std::fs::read_to_string(&entry).unwrap();
    prune_entry(&entry, &raw, Some(&path));
    assert!(entry.exists());
}

#[test]
fn recovery_registry_releases_inactive_directory_capacity_and_preserves_other_hosts() {
    let fixture = Fixture::new();
    let c = controller(&fixture, &fixture.session_dir.join("registry.jsonl"), true);
    let another_host = controller(&fixture, &fixture.session_dir.join("other.jsonl"), true);
    let dead_cancel = TraceUploadCancel::new();
    let live_cancel = TraceUploadCancel::new();
    let mut recovered = RecoveryRuns::new();
    for n in 0..MAX_CONTROLLERS - 1 {
        recovered.insert(
            PathBuf::from(format!("synthetic-inactive-{n}")),
            RecoveryRun {
                cancel: dead_cancel.clone(),
                ..RecoveryRun::new(vec![])
            },
        );
    }
    recovered.insert(
        fixture.agent_dir,
        RecoveryRun {
            cancel: live_cancel.clone(),
            ..RecoveryRun::new(vec![Arc::downgrade(&c), Arc::downgrade(&another_host)])
        },
    );
    let registration = Registration::of(&c);
    drop(c);
    let _ = retain_live_recoveries(
        &mut recovered,
        &[registration, Registration::of(&another_host)],
    );
    assert!(dead_cancel.is_cancelled());
    assert!(!live_cancel.is_cancelled());
    assert_eq!(recovered.len(), 1);
    drop(another_host);
    let _ = retain_live_recoveries(&mut recovered, &[]);
    assert!(live_cancel.is_cancelled());
    assert!(recovered.is_empty());
}

#[tokio::test(start_paused = true)]
async fn incomplete_recovery_cooldown_is_bounded_and_shutdown_cancellable() {
    let state = Arc::new(std::sync::atomic::AtomicU8::new(1));
    let cancel = TraceUploadCancel::new();
    let task_state = state.clone();
    let task_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        settle_recovery_run(&task_state, false, None, &task_cancel).await;
    });
    tokio::task::yield_now().await;
    tokio::time::advance(MIN_INTERVAL.checked_sub(DEBOUNCE).unwrap()).await;
    assert_eq!(state.load(Ordering::Acquire), 1);
    tokio::time::advance(DEBOUNCE).await;
    task.await.unwrap();
    assert_eq!(state.load(Ordering::Acquire), 0);
    state.store(1, Ordering::Release);
    let task_state = state.clone();
    let task_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        settle_recovery_run(&task_state, false, None, &task_cancel).await;
    });
    tokio::task::yield_now().await;
    let began = Instant::now();
    cancel.cancel();
    task.await.unwrap();
    assert_eq!(Instant::now(), began);
    assert_eq!(state.load(Ordering::Acquire), 0);
}

#[test]
fn oversized_cursor_records_fail_closed_with_bounded_reads() {
    let fixture = Fixture::new();
    let path = fixture.write_session("large-cursor.jsonl", "large-cursor");
    record_agent_trace_outbox_upload(
        &fixture.agent_dir,
        &path,
        TraceUploadSignature::of(&path).unwrap(),
    )
    .unwrap();
    let entry = agent_trace_outbox_entry_path(&fixture.agent_dir, &path);
    let mut value: Value = serde_json::from_str(&std::fs::read_to_string(&entry).unwrap()).unwrap();
    value["padding"] = json!(" ".repeat(64 * 1024));
    std::fs::write(entry, value.to_string()).unwrap();
    assert_eq!(
        read_agent_trace_outbox_entry(&fixture.agent_dir, &path),
        None
    );
}

#[tokio::test(start_paused = true)]
async fn retirement_racing_completion_survives_and_preserves_the_retry_deadline() {
    let run = RecoveryRun::new(vec![]);
    run.state.store(1, Ordering::Release);
    let complete = !run.replay_needed.load(Ordering::Acquire);
    run.replay_needed.store(true, Ordering::Release);
    let deadline = Instant::now() + Duration::from_secs(120);
    *run.not_before.lock().unwrap() = Some(deadline);
    settle_recovery_run(&run.state, complete, Some(deadline), &run.cancel).await;
    assert_eq!(run.state.load(Ordering::Acquire), 2);
    rearm_retired_recovery(&run);
    rearm_retired_recovery(&run);
    assert_eq!(run.state.load(Ordering::Acquire), 1);
    tokio::task::yield_now().await;
    tokio::time::advance(MIN_INTERVAL).await;
    assert_eq!(run.state.load(Ordering::Acquire), 1);
    tokio::time::advance(MIN_INTERVAL).await;
    tokio::task::yield_now().await;
    assert_eq!(run.state.load(Ordering::Acquire), 0);
    assert!(
        run.replay_needed.load(Ordering::Acquire),
        "only a new sweep may clear retirement intent"
    );
}

