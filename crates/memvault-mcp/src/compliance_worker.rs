//! Background consumer for durable SessionEnd compliance jobs.
//!
//! Codex clamps `SessionEnd` commands to three seconds, so the hook must only
//! enqueue. This worker runs in the long-lived REST server, reads the host
//! transcript, and performs the LLM judgment outside that timeout budget.
//! Every failure path degrades to an explicit `unknown` verdict instead of
//! leaving rows pending or inventing compliance.

use std::future::Future;
use std::io::SeekFrom;
use std::sync::Arc;
use std::time::{Duration, Instant};

use memvault_core::compliance::{
    ComplianceFinalizeJob, ComplianceStatus, ComplianceStore, JobLease, LeaseWrite,
};
use memvault_core::compliance_judge::{self, FinalizeAttempt, FinalizeReport};
use memvault_core::llm_extractor::LazyLlmExtractor;
use memvault_core::storage::MemoryStore;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tracing::{debug, info, warn};

/// A job may make one LLM call per injected rule. The old `SessionEnd`
/// three-second budget is gone; this deadline bounds a wedged provider while
/// still allowing normal remote-model latency.
pub const DEFAULT_JOB_TIMEOUT_SECS: u64 = 90;
const DEFAULT_POLL_INTERVAL_MS: u64 = 1_000;
const STALE_RUNNING_AFTER_SECS: i64 = 600;
/// Keep a live worker's lease comfortably inside the stale threshold. A job
/// that is still making progress must never be reclaimed just because one
/// LLM call (or a long judge batch) takes longer than the threshold.
const LEASE_HEARTBEAT_SECS: u64 = 30;
/// Reap during the worker loop as well as at startup. A service restarted
/// before a crashed job crossed the 600s threshold must recover it later
/// without another manual restart.
const REAP_INTERVAL_SECS: u64 = 60;
const MAX_TRANSCRIPT_TAIL_BYTES: u64 = 512 * 1024;

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

/// Per-job deadline. Set `MEMVAULT_COMPLIANCE_WORKER_TIMEOUT_SECS` to tune it;
/// values of zero fall back to the default rather than disabling the safety
/// net.
pub fn job_timeout_from_env() -> Duration {
    Duration::from_secs(env_u64(
        "MEMVAULT_COMPLIANCE_WORKER_TIMEOUT_SECS",
        DEFAULT_JOB_TIMEOUT_SECS,
    ))
}

fn poll_interval_from_env() -> Duration {
    Duration::from_millis(env_u64(
        "MEMVAULT_COMPLIANCE_WORKER_POLL_MS",
        DEFAULT_POLL_INTERVAL_MS,
    ))
}

/// Read only the tail of a rollout. Codex transcripts can be very large; the
/// compliance judge only needs the final turns anyway.
async fn read_transcript(path: &str) -> Option<String> {
    let mut file = tokio::fs::File::open(path).await.ok()?;
    let len = file.metadata().await.ok()?.len();
    if len > MAX_TRANSCRIPT_TAIL_BYTES {
        file.seek(SeekFrom::Start(len - MAX_TRANSCRIPT_TAIL_BYTES))
            .await
            .ok()?;
    }
    let mut bytes = Vec::with_capacity(len.min(MAX_TRANSCRIPT_TAIL_BYTES) as usize);
    file.read_to_end(&mut bytes).await.ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

async fn transcript_for_job(job: &ComplianceFinalizeJob) -> Option<String> {
    if let Some(tail) = job
        .transcript_tail
        .as_deref()
        .filter(|tail| !tail.trim().is_empty())
    {
        return Some(tail.to_string());
    }
    match job.transcript_path.as_deref() {
        Some(path) => read_transcript(path).await,
        None => None,
    }
}

/// Run `work` while extending the job lease on a fixed interval. This keeps
/// `updated_at` fresh for the stale-running reaper, so a slow-but-alive job
/// is not requeued and judged a second time. The heartbeat is in-process
/// (no extra task) and stops as soon as the work future resolves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeaseRun<T> {
    Completed(T),
    LeaseLost,
}

async fn with_lease_heartbeat<F, T>(
    compliance: &ComplianceStore,
    job_id: i64,
    lease_token: &str,
    interval: Duration,
    work: F,
) -> LeaseRun<T>
where
    F: Future<Output = T>,
{
    tokio::pin!(work);
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            result = &mut work => return LeaseRun::Completed(result),
            _ = ticker.tick() => {
                match compliance.heartbeat_finalize_job(job_id, lease_token).await {
                    Ok(true) => {}
                    Ok(false) => {
                        // Dropping `work` cancels the in-flight judge future.
                        // The leased write methods are the second barrier:
                        // even a write racing this tick is rejected by SQL.
                        warn!(
                            job_id,
                            "compliance job lease was lost while work was still running; cancelling"
                        );
                        return LeaseRun::LeaseLost;
                    }
                    Err(error) => warn!(
                        job_id,
                        error = %error,
                        "failed to extend compliance job lease"
                    ),
                }
            }
        }
    }
}

/// Whether the periodic reaper is due. Split out so the worker loop's
/// scheduling can be tested without sleeping for the production interval.
fn reap_due(last_reap: Instant, now: Instant, interval: Duration) -> bool {
    now.saturating_duration_since(last_reap) >= interval
}

/// Reap stale `running` jobs when the interval has elapsed. Returns 0 when
/// this call was not due, otherwise the number of jobs requeued.
async fn maybe_reap_stale_jobs(
    compliance: &ComplianceStore,
    stale_after: chrono::Duration,
    interval: Duration,
    last_reap: &mut Instant,
) -> anyhow::Result<usize> {
    let now = Instant::now();
    if !reap_due(*last_reap, now, interval) {
        return Ok(0);
    }
    // Advance before the database call: a transient error should wait for
    // the next interval instead of retrying on every one-second poll.
    *last_reap = now;
    Ok(compliance.reap_stale_finalize_jobs(stale_after).await?)
}

async fn finalize_unknown(
    compliance: &ComplianceStore,
    job: &ComplianceFinalizeJob,
    lease: JobLease<'_>,
    reason: &str,
) -> anyhow::Result<LeaseWrite> {
    Ok(compliance
        .finalize_pending_for_host_session_upto_leased(
            lease,
            &job.host_session_id,
            job.batch_max_seq,
            ComplianceStatus::Unknown,
            None,
            Some(reason),
        )
        .await?)
}

async fn finish_after_unknown(
    compliance: &ComplianceStore,
    job: &ComplianceFinalizeJob,
    lease: JobLease<'_>,
    reason: &str,
) -> anyhow::Result<()> {
    match finalize_unknown(compliance, job, lease, reason).await {
        Ok(LeaseWrite::Applied(finalized)) => {
            match compliance
                .complete_finalize_job(
                    job.id,
                    lease.lease_token,
                    Some(&format!(
                        "{reason}; {finalized} pending row(s) closed as unknown"
                    )),
                )
                .await?
            {
                Some(status) => info!(
                    job_id = job.id,
                    agent = %job.agent_id,
                    finalized,
                    reason,
                    %status,
                    "compliance job completed with unknown fallback"
                ),
                None => warn!(
                    job_id = job.id,
                    "compliance job lease was lost before the unknown fallback completed"
                ),
            }
            Ok(())
        }
        Ok(LeaseWrite::LeaseLost) => {
            warn!(
                job_id = job.id,
                reason, "compliance job lease was lost before the unknown fallback"
            );
            Ok(())
        }
        Err(error) => {
            let detail = format!("{reason}; unknown fallback failed: {error}");
            match compliance
                .fail_finalize_job(job.id, lease.lease_token, &detail)
                .await
            {
                Ok(true) => {}
                Ok(false) => warn!(
                    job_id = job.id,
                    "compliance job lease was lost before the failure could be recorded"
                ),
                Err(fail_error) => warn!(
                    job_id = job.id,
                    error = %fail_error,
                    "failed to record compliance job failure"
                ),
            }
            Err(error)
        }
    }
}

/// Process at most one queued job. Returns `true` when a job was claimed,
/// whether it ultimately produced verdicts or an explicit unknown fallback.
pub async fn process_next_compliance_job(
    store: &dyn MemoryStore,
    compliance: &ComplianceStore,
    llm: &LazyLlmExtractor,
    timeout: Duration,
) -> anyhow::Result<bool> {
    let Some(job) = compliance.claim_next_finalize_job().await? else {
        return Ok(false);
    };
    let job_id = job.id;
    let Some(lease_token) = job.lease_token.clone() else {
        anyhow::bail!("claimed compliance job {job_id} has no lease token");
    };
    let heartbeat_token = lease_token.clone();
    let outcome = with_lease_heartbeat(
        compliance,
        job_id,
        &heartbeat_token,
        Duration::from_secs(LEASE_HEARTBEAT_SECS),
        async move {
            let lease = JobLease {
                job_id,
                lease_token: &lease_token,
            };
            let pending = compliance
                .pending_compliance_for_host_session_upto(&job.host_session_id, job.batch_max_seq)
                .await?;
            if pending.is_empty() {
                if compliance
                    .complete_finalize_job(job.id, &lease_token, Some("no pending compliance rows"))
                    .await?
                    .is_none()
                {
                    warn!(
                        job_id = job.id,
                        "compliance job lease was lost before an empty batch completed"
                    );
                }
                debug!(job_id = job.id, "compliance job had no pending rows");
                return Ok(true);
            }

            let Some(transcript) = transcript_for_job(&job).await else {
                finish_after_unknown(
                    compliance,
                    &job,
                    lease,
                    "compliance worker could not read the session transcript",
                )
                .await?;
                return Ok(true);
            };
            if transcript.trim().is_empty() {
                finish_after_unknown(
                    compliance,
                    &job,
                    lease,
                    "compliance worker found an empty session transcript",
                )
                .await?;
                return Ok(true);
            }

            let judged = tokio::time::timeout(timeout, async {
                let judge = llm.get().await;
                let has_judge = judge.is_some();
                let mut total = FinalizeReport::default();
                loop {
                    let remaining = compliance
                        .pending_compliance_for_host_session_upto(
                            &job.host_session_id,
                            job.batch_max_seq,
                        )
                        .await?
                        .len();
                    if remaining == 0 {
                        break;
                    }
                    let attempt = compliance_judge::finalize_host_session_upto_leased(
                        compliance,
                        store,
                        judge.as_deref(),
                        &job.host_session_id,
                        &transcript,
                        job.batch_max_seq,
                        lease,
                    )
                    .await?;
                    let batch = match attempt {
                        FinalizeAttempt::Completed(batch) => batch,
                        FinalizeAttempt::LeaseLost => {
                            return Ok((has_judge, FinalizeAttempt::LeaseLost));
                        }
                    };
                    total.judged += batch.judged;
                    total.unknown += batch.unknown;
                    // The judge caps one pass at 20 rows; loop for larger batches.
                    // A zero-progress pass can only mean there was nothing left to
                    // judge, so stop rather than spin.
                    if batch.judged + batch.unknown == 0 {
                        break;
                    }
                }
                Ok::<_, memvault_core::error::MemVaultError>((
                    has_judge,
                    FinalizeAttempt::Completed(total),
                ))
            })
            .await;

            match judged {
                Ok(Ok((has_judge, FinalizeAttempt::Completed(report)))) => {
                    let remaining = compliance
                        .pending_compliance_for_host_session_upto(
                            &job.host_session_id,
                            job.batch_max_seq,
                        )
                        .await?
                        .len();
                    if remaining > 0 {
                        warn!(
                            job_id = job.id,
                            remaining,
                            "compliance worker made no further progress; applying unknown fallback"
                        );
                        finish_after_unknown(
                            compliance,
                            &job,
                            lease,
                            "compliance worker stopped with pending rows; closed as unknown",
                        )
                        .await?;
                        return Ok(true);
                    }
                    let note = if has_judge {
                        None
                    } else {
                        Some(
                            "no LLM compliance judge configured; pending rows finalized as unknown",
                        )
                    };
                    if compliance
                        .complete_finalize_job(job.id, &lease_token, note)
                        .await?
                        .is_none()
                    {
                        warn!(
                            job_id = job.id,
                            "compliance job lease was lost before completion"
                        );
                    }
                    info!(
                        job_id = job.id,
                        agent = %job.agent_id,
                        judged = report.judged,
                        unknown = report.unknown,
                        "compliance job completed"
                    );
                    Ok(true)
                }
                Ok(Ok((_has_judge, FinalizeAttempt::LeaseLost))) => {
                    warn!(
                        job_id = job.id,
                        "compliance job lease was lost while judging; stopped before completion"
                    );
                    Ok(true)
                }
                Ok(Err(error)) => {
                    warn!(
                        job_id = job.id,
                        error = %error,
                        "compliance job failed; applying unknown fallback"
                    );
                    finish_after_unknown(
                        compliance,
                        &job,
                        lease,
                        "compliance judge failed; pending rows closed as unknown",
                    )
                    .await?;
                    Ok(true)
                }
                Err(_) => {
                    warn!(
                        job_id = job.id,
                        timeout_secs = timeout.as_secs_f64(),
                        "compliance job deadline exceeded; applying unknown fallback"
                    );
                    finish_after_unknown(
                        compliance,
                        &job,
                        lease,
                        "compliance worker deadline exceeded; pending rows closed as unknown",
                    )
                    .await?;
                    Ok(true)
                }
            }
        },
    )
    .await;

    match outcome {
        LeaseRun::Completed(result) => result,
        LeaseRun::LeaseLost => Ok(true),
    }
}

/// Spawn the single background consumer owned by the REST server process.
/// stdio MCP processes are short-lived, so they deliberately do not start a
/// second consumer that would compete for the same durable queue.
pub fn spawn_compliance_worker(
    store: Arc<dyn MemoryStore>,
    compliance: Arc<ComplianceStore>,
    llm: LazyLlmExtractor,
) -> tokio::task::JoinHandle<()> {
    let timeout = job_timeout_from_env();
    let poll_interval = poll_interval_from_env();
    tokio::spawn(async move {
        let stale_after = chrono::Duration::seconds(STALE_RUNNING_AFTER_SECS);
        let reap_interval = Duration::from_secs(REAP_INTERVAL_SECS);
        let mut last_reap = Instant::now();
        match compliance.reap_stale_finalize_jobs(stale_after).await {
            Ok(0) => {}
            Ok(count) => warn!(count, "requeued stale compliance finalize jobs"),
            Err(error) => warn!(error = %error, "failed to reap stale compliance jobs"),
        }

        loop {
            match maybe_reap_stale_jobs(
                compliance.as_ref(),
                stale_after,
                reap_interval,
                &mut last_reap,
            )
            .await
            {
                Ok(0) => {}
                Ok(count) => warn!(count, "requeued stale compliance finalize jobs"),
                Err(error) => warn!(error = %error, "failed to reap stale compliance jobs"),
            }
            match process_next_compliance_job(store.as_ref(), compliance.as_ref(), &llm, timeout)
                .await
            {
                Ok(true) => {}
                Ok(false) => tokio::time::sleep(poll_interval).await,
                Err(error) => {
                    warn!(error = %error, "compliance worker iteration failed");
                    tokio::time::sleep(poll_interval).await;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use memvault_core::error::Result as CoreResult;
    use memvault_core::extractor::ExtractedMemory;
    use memvault_core::llm_extractor::LlmExtractor;
    use memvault_core::models::{Memory, MemoryType, Priority, SourceAgent};
    use memvault_core::storage::sqlite::SqliteStore;
    use std::path::Path;

    struct FixedJudge(&'static str);

    #[async_trait]
    impl LlmExtractor for FixedJudge {
        async fn extract(&self, _context: &str) -> CoreResult<Vec<ExtractedMemory>> {
            Ok(Vec::new())
        }

        async fn json_chat(&self, _system: &str, _user: &str) -> CoreResult<Option<String>> {
            Ok(Some(self.0.to_string()))
        }
    }

    struct SlowJudge;

    #[async_trait]
    impl LlmExtractor for SlowJudge {
        async fn extract(&self, _context: &str) -> CoreResult<Vec<ExtractedMemory>> {
            Ok(Vec::new())
        }

        async fn json_chat(&self, _system: &str, _user: &str) -> CoreResult<Option<String>> {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok(Some(r#"{"status":"followed"}"#.to_string()))
        }
    }

    struct Fixture {
        store: Arc<SqliteStore>,
        compliance: Arc<ComplianceStore>,
        path: std::path::PathBuf,
        transcript_path: Option<std::path::PathBuf>,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
            if let Some(path) = &self.transcript_path {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    async fn fixture(transcript: Option<&str>) -> Fixture {
        let path = std::env::temp_dir().join(format!(
            "memvault-compliance-worker-{}.db",
            uuid::Uuid::new_v4()
        ));
        let store = Arc::new(SqliteStore::new(Path::new(&path)).unwrap());
        let compliance = ComplianceStore::new(path.to_str().unwrap()).unwrap();
        let mut memory = Memory::new(
            MemoryType::Preference,
            "Always reply to the user in Chinese.".to_string(),
            Priority::Must,
            SourceAgent {
                id: "codex".to_string(),
                agent_type: "coding-assistant".to_string(),
                session_id: None,
            },
        );
        memory.human_reviewed = true;
        store.save(memory).await.unwrap();
        compliance
            .record_injection_with_host_session(
                "inject-1",
                &store.list(None, 1, 0).await.unwrap()[0].id,
                &Priority::Must,
                "codex",
                Some("host-worker"),
            )
            .await
            .unwrap();

        let transcript_path = transcript.map(|text| {
            let path = std::env::temp_dir()
                .join(format!("memvault-rollout-{}.jsonl", uuid::Uuid::new_v4()));
            std::fs::write(&path, text).unwrap();
            path
        });
        compliance
            .enqueue_finalize_job(
                "host-worker",
                "codex",
                transcript_path
                    .as_ref()
                    .map(|path| path.to_string_lossy().to_string())
                    .as_deref(),
                None,
            )
            .await
            .unwrap();

        Fixture {
            store,
            compliance,
            path,
            transcript_path,
        }
    }

    async fn process(fixture: &Fixture, judge: Option<Arc<dyn LlmExtractor>>, ms: u64) -> bool {
        process_next_compliance_job(
            fixture.store.as_ref(),
            fixture.compliance.as_ref(),
            &LazyLlmExtractor::fixed(judge),
            Duration::from_millis(ms),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn worker_records_followed_verdict() {
        let fixture = fixture(Some("assistant: 我用中文回答。")).await;
        assert!(
            process(
                &fixture,
                Some(Arc::new(FixedJudge(
                    r#"{"status":"followed","evidence":"used Chinese"}"#
                ))),
                1_000,
            )
            .await
        );
        let report = fixture.compliance.get_report("inject-1").await.unwrap();
        assert_eq!(report.must_followed, 1);
        assert_eq!(report.pending, 0);
        assert_eq!(report.compliance_rate, Some(1.0));
    }

    #[tokio::test]
    async fn worker_without_provider_records_unknown() {
        let fixture = fixture(Some("assistant: 我用中文回答。")).await;
        assert!(process(&fixture, None, 1_000).await);
        let report = fixture.compliance.get_report("inject-1").await.unwrap();
        assert_eq!(report.pending, 0);
        assert_eq!(report.must_followed, 0);
        assert_eq!(
            report.compliance_rate, None,
            "no judge must never look like 100% compliance"
        );
    }

    #[tokio::test]
    async fn worker_timeout_records_unknown() {
        let fixture = fixture(Some("assistant: 我用中文回答。")).await;
        assert!(process(&fixture, Some(Arc::new(SlowJudge)), 10).await);
        let report = fixture.compliance.get_report("inject-1").await.unwrap();
        assert_eq!(report.pending, 0);
        assert_eq!(report.compliance_rate, None);
        let duplicate = fixture
            .compliance
            .enqueue_finalize_job("host-worker", "codex", None, None)
            .await
            .unwrap();
        assert_eq!(duplicate.job_id, 1);
        assert!(!duplicate.inserted);
        assert_eq!(
            duplicate.status,
            memvault_core::compliance::FinalizeJobStatus::Done
        );
    }

    #[tokio::test]
    async fn worker_invalid_judge_output_records_unknown() {
        let fixture = fixture(Some("assistant: 我用中文回答。")).await;
        assert!(process(&fixture, Some(Arc::new(FixedJudge("not-json"))), 1_000,).await);
        let report = fixture.compliance.get_report("inject-1").await.unwrap();
        assert_eq!(report.pending, 0);
        assert_eq!(report.compliance_rate, None);
    }

    #[tokio::test]
    async fn missing_transcript_records_unknown() {
        let fixture = fixture(None).await;
        assert!(
            process(
                &fixture,
                Some(Arc::new(FixedJudge(r#"{"status":"followed"}"#))),
                1_000,
            )
            .await
        );
        let report = fixture.compliance.get_report("inject-1").await.unwrap();
        assert_eq!(report.pending, 0);
        assert_eq!(report.compliance_rate, None);
    }

    #[tokio::test]
    async fn worker_drains_more_than_one_judge_batch() {
        let fixture = fixture(Some("assistant: 我用中文回答。")).await;
        let memory_id = fixture.store.list(None, 1, 0).await.unwrap()[0].id.clone();
        // The judge caps one pass at 20 rows. Add enough duplicates that only
        // the worker's loop (not a single finalize call) closes all of them.
        for _ in 0..24 {
            fixture
                .compliance
                .record_injection_with_host_session(
                    "inject-batch",
                    &memory_id,
                    &Priority::Must,
                    "codex",
                    Some("host-worker"),
                )
                .await
                .unwrap();
        }
        assert!(
            process(
                &fixture,
                Some(Arc::new(FixedJudge(r#"{"status":"followed"}"#))),
                5_000,
            )
            .await
        );
        assert!(
            fixture
                .compliance
                .pending_compliance_for_host_session("host-worker")
                .await
                .unwrap()
                .is_empty(),
            "worker must loop past the per-pass judge cap"
        );
    }

    /// Regression (#7): a consumer that restarts before a crashed job's
    /// 600s threshold must still recover it later. Reaping now happens in
    /// the long-running loop, not only at startup; the same store instance
    /// can requeue the stale job without a second restart.
    #[tokio::test]
    async fn periodic_reaper_requeues_stale_running_job_without_restart() {
        let fixture = fixture(Some("done")).await;
        let claimed = fixture
            .compliance
            .claim_next_finalize_job()
            .await
            .unwrap()
            .expect("fixture job must be claimable");

        let mut last_reap = Instant::now();
        let skipped = maybe_reap_stale_jobs(
            fixture.compliance.as_ref(),
            chrono::Duration::zero(),
            Duration::from_secs(3600),
            &mut last_reap,
        )
        .await
        .unwrap();
        assert_eq!(skipped, 0, "a reap before the interval is not due");
        assert!(
            fixture
                .compliance
                .claim_next_finalize_job()
                .await
                .unwrap()
                .is_none(),
            "the still-fresh running job must not be requeued early"
        );

        let reaped = maybe_reap_stale_jobs(
            fixture.compliance.as_ref(),
            chrono::Duration::zero(),
            Duration::ZERO,
            &mut last_reap,
        )
        .await
        .unwrap();
        assert_eq!(reaped, 1, "the loop must requeue the stale job");
        let reclaimed = fixture
            .compliance
            .claim_next_finalize_job()
            .await
            .unwrap()
            .expect("the requeued job must be claimable without a restart");
        assert_eq!(reclaimed.id, claimed.id);
        assert_eq!(reclaimed.attempts, 2);
    }

    /// Companion to the reaper: a legitimately slow job keeps extending its
    /// lease, so the periodic sweep does not mistake "still working" for
    /// "crashed" and run the same judge twice.
    #[tokio::test]
    async fn heartbeat_keeps_a_slow_job_out_of_the_stale_reaper() {
        let fixture = fixture(Some("done")).await;
        let claimed = fixture
            .compliance
            .claim_next_finalize_job()
            .await
            .unwrap()
            .expect("fixture job must be claimable");
        let lease_token = claimed.lease_token.clone().unwrap();

        let result = with_lease_heartbeat(
            fixture.compliance.as_ref(),
            claimed.id,
            &lease_token,
            Duration::from_millis(20),
            async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                7
            },
        )
        .await;
        assert_eq!(result, LeaseRun::Completed(7));

        let reaped = fixture
            .compliance
            .reap_stale_finalize_jobs(chrono::Duration::milliseconds(50))
            .await
            .unwrap();
        assert_eq!(
            reaped, 0,
            "heartbeats must keep an actively running job out of the stale sweep"
        );
    }

    /// When a newer worker owns the job, the heartbeat must stop the old
    /// worker's future instead of letting it finish the judge call and write
    /// verdicts for rows it no longer owns.
    #[tokio::test]
    async fn lost_lease_cancels_in_flight_work() {
        let fixture = fixture(Some("done")).await;
        let worker_a = fixture
            .compliance
            .claim_next_finalize_job()
            .await
            .unwrap()
            .expect("fixture job must be claimable");
        fixture
            .compliance
            .reap_stale_finalize_jobs(chrono::Duration::zero())
            .await
            .unwrap();
        let worker_b = fixture
            .compliance
            .claim_next_finalize_job()
            .await
            .unwrap()
            .expect("replacement worker must claim the job");
        let token_b = worker_b.lease_token.clone().unwrap();

        let started = Instant::now();
        let outcome = with_lease_heartbeat(
            fixture.compliance.as_ref(),
            worker_a.id,
            worker_a.lease_token.as_deref().unwrap(),
            Duration::from_millis(10),
            async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                7
            },
        )
        .await;
        assert!(matches!(outcome, LeaseRun::LeaseLost));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "lease loss must cancel the work promptly, not wait for it to finish"
        );

        assert_eq!(
            fixture
                .compliance
                .complete_finalize_job(worker_b.id, &token_b, None)
                .await
                .unwrap(),
            Some(memvault_core::compliance::FinalizeJobStatus::Queued),
            "the replacement still owns and can finish the job"
        );
    }

    #[tokio::test]
    async fn stale_worker_unknown_fallback_is_a_noop() {
        let fixture = fixture(Some("done")).await;
        let worker_a = fixture
            .compliance
            .claim_next_finalize_job()
            .await
            .unwrap()
            .expect("fixture job must be claimable");
        fixture
            .compliance
            .reap_stale_finalize_jobs(chrono::Duration::zero())
            .await
            .unwrap();
        let worker_b = fixture
            .compliance
            .claim_next_finalize_job()
            .await
            .unwrap()
            .expect("replacement worker must claim the job");
        let token_b = worker_b.lease_token.clone().unwrap();

        finish_after_unknown(
            fixture.compliance.as_ref(),
            &worker_a,
            JobLease {
                job_id: worker_a.id,
                lease_token: worker_a.lease_token.as_deref().unwrap(),
            },
            "stale fallback",
        )
        .await
        .unwrap();

        assert_eq!(
            fixture
                .compliance
                .get_report("inject-1")
                .await
                .unwrap()
                .pending,
            1,
            "the stale fallback must not close the replacement's pending row"
        );
        assert_eq!(
            fixture
                .compliance
                .complete_finalize_job(worker_b.id, &token_b, None)
                .await
                .unwrap(),
            Some(memvault_core::compliance::FinalizeJobStatus::Queued)
        );
    }

    #[tokio::test]
    async fn idle_worker_returns_false() {
        let fixture = fixture(Some("done")).await;
        assert!(process(&fixture, None, 1_000).await);
        assert!(!process(&fixture, None, 1_000).await);
    }
}
