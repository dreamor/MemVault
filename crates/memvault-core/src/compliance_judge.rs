//! Session-end compliance finalization.
//!
//! `record_injection` only knows that a memory was injected. Whether the
//! agent actually followed it used to depend entirely on an external
//! `report_compliance` call, which hook-based hosts never made — every
//! `compliance_events` row stayed `pending` forever and the aggregate report
//! looked like a vacuous 100%.
//!
//! This module closes that loop at the host's `SessionEnd` event. When an LLM
//! extractor is configured it judges each injected memory against the
//! session transcript (`followed` / `violated` / `unknown`). When none is
//! configured it records `unknown` with an explicit reason instead of
//! fabricating a verdict or leaving the row pending forever.

use crate::bench::parse_field;
use crate::compliance::{ComplianceStatus, ComplianceStore};
use crate::error::Result;
use crate::llm_extractor::LlmExtractor;
use crate::storage::MemoryStore;
use tracing::{debug, warn};

/// Keep the prompt bounded: a long rollout can be hundreds of MB, while the
/// `SessionEnd` hook has a short timeout. The last turns are where a rule is
/// normally followed or violated.
pub const MAX_TRANSCRIPT_CHARS: usize = 12_000;

/// One session start injects at most `max_memories` (default 8); this bound
/// only protects against a corrupted/hand-edited backlog and keeps one
/// SessionEnd hook from making an unbounded number of LLM calls.
pub const MAX_COMPLIANCE_JUDGED_PER_SESSION: usize = 20;

const COMPLIANCE_JUDGE_SYSTEM: &str = "You audit whether an AI agent followed a memory rule \
that was injected into its context at session start. You are given the rule and the session \
transcript. Decide one of: followed (clear evidence the assistant complied), violated (clear \
evidence the assistant did the opposite), or unknown (no clear evidence either way). Judge \
the assistant's behavior, never the user's words alone; a user quoting a rule is not evidence \
the assistant followed it. Reply with JSON {\"status\":\"followed\"|\"violated\"|\"unknown\",\
\"evidence\":string,\"reason\":string}.";

#[derive(Debug, Default, Clone, Copy)]
pub struct FinalizeReport {
    /// Rows that received a real followed/violated verdict.
    pub judged: usize,
    /// Rows closed honestly as `unknown`.
    pub unknown: usize,
}

fn transcript_tail(text: &str, max_chars: usize) -> &str {
    let chars = text.chars().count();
    if chars <= max_chars {
        return text;
    }
    let skip = chars - max_chars;
    let byte = text
        .char_indices()
        .nth(skip)
        .map(|(index, _)| index)
        .unwrap_or(0);
    &text[byte..]
}

/// Judge one injected memory against the session transcript. `Ok(None)` means
/// the judge could not produce a usable verdict; callers must record
/// `unknown`, never guess.
async fn judge_one(
    judge: &dyn LlmExtractor,
    rule: &str,
    transcript: &str,
) -> Result<Option<(ComplianceStatus, Option<String>, Option<String>)>> {
    let user = format!(
        "Rule injected at session start:\n{rule}\n\nSession transcript (tail):\n{}",
        transcript_tail(transcript, MAX_TRANSCRIPT_CHARS)
    );
    let Some(raw) = judge.json_chat(COMPLIANCE_JUDGE_SYSTEM, &user).await? else {
        return Ok(None);
    };
    let Some(status_str) = parse_field(&raw, "status") else {
        return Ok(None);
    };
    let status = match status_str.to_ascii_lowercase().as_str() {
        "followed" => ComplianceStatus::Followed,
        "violated" => ComplianceStatus::Violated,
        "unknown" => ComplianceStatus::Unknown,
        _ => return Ok(None),
    };
    Ok(Some((
        status,
        parse_field(&raw, "evidence"),
        parse_field(&raw, "reason"),
    )))
}

/// Close every pending compliance row for `host_session_id`.
///
/// Best-effort like reflection/effectiveness: one bad row must not abort the
/// rest, and a missing memory is recorded as `unknown` rather than silently
/// dropped. Returns how many rows got a real verdict vs. an honest unknown.
pub async fn finalize_host_session(
    compliance: &ComplianceStore,
    store: &dyn MemoryStore,
    judge: Option<&dyn LlmExtractor>,
    host_session_id: &str,
    transcript: &str,
) -> Result<FinalizeReport> {
    let pending = compliance
        .pending_compliance_for_host_session(host_session_id)
        .await?;
    if pending.is_empty() {
        return Ok(FinalizeReport::default());
    }

    let Some(judge) = judge else {
        let finalized = compliance
            .finalize_pending_for_host_session(
                host_session_id,
                ComplianceStatus::Unknown,
                None,
                Some(
                    "session ended without an LLM compliance judge; \
                     configure MEMVAULT_LLM_EXTRACTION_PROVIDER to classify followed/violated",
                ),
            )
            .await?;
        debug!(
            host_session_id,
            finalized, "compliance finalized as unknown (no LLM judge)"
        );
        return Ok(FinalizeReport {
            judged: 0,
            unknown: finalized,
        });
    };

    let mut report = FinalizeReport::default();
    for (index, row) in pending.iter().enumerate() {
        if index >= MAX_COMPLIANCE_JUDGED_PER_SESSION {
            // Leave the overflow pending; the next finalize call can pick it
            // up without pretending it was judged.
            debug!(
                host_session_id,
                remaining = pending.len() - index,
                "compliance judging limit reached"
            );
            break;
        }

        let memory = match store.get(&row.memory_id).await {
            Ok(memory) => memory,
            Err(error) => {
                warn!(
                    memory_id = %row.memory_id,
                    error = %error,
                    "compliance judge skipped a missing memory"
                );
                let _ = compliance
                    .report_with_reason(
                        &row.inject_session_id,
                        &row.memory_id,
                        ComplianceStatus::Unknown,
                        None,
                        Some("memory no longer exists; cannot evaluate compliance"),
                    )
                    .await;
                report.unknown += 1;
                continue;
            }
        };
        let rule = memory.instruction.as_deref().unwrap_or(&memory.content);

        match judge_one(judge, rule, transcript).await {
            Ok(Some((status, evidence, reason))) => {
                let resolved = compliance
                    .report_with_reason(
                        &row.inject_session_id,
                        &row.memory_id,
                        status.clone(),
                        evidence.as_deref(),
                        reason.as_deref(),
                    )
                    .await;
                match resolved {
                    Ok(()) => {
                        if status == ComplianceStatus::Unknown {
                            report.unknown += 1;
                        } else {
                            report.judged += 1;
                        }
                    }
                    Err(error) => {
                        warn!(
                            error = %error,
                            memory_id = %row.memory_id,
                            "failed to record compliance verdict"
                        );
                        report.unknown += 1;
                    }
                }
            }
            Ok(None) => {
                let _ = compliance
                    .report_with_reason(
                        &row.inject_session_id,
                        &row.memory_id,
                        ComplianceStatus::Unknown,
                        None,
                        Some("compliance judge returned no usable verdict"),
                    )
                    .await;
                report.unknown += 1;
            }
            Err(error) => {
                warn!(
                    error = %error,
                    memory_id = %row.memory_id,
                    "compliance judge call failed"
                );
                let _ = compliance
                    .report_with_reason(
                        &row.inject_session_id,
                        &row.memory_id,
                        ComplianceStatus::Unknown,
                        None,
                        Some("compliance judge call failed"),
                    )
                    .await;
                report.unknown += 1;
            }
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extractor::ExtractedMemory;
    use crate::models::{Memory, MemoryType, Priority, SourceAgent};
    use crate::storage::sqlite::SqliteStore;
    use async_trait::async_trait;
    use std::path::Path;
    use std::sync::Arc;

    struct FixedJudge(&'static str);

    #[async_trait]
    impl LlmExtractor for FixedJudge {
        async fn extract(&self, _context: &str) -> Result<Vec<ExtractedMemory>> {
            Ok(Vec::new())
        }

        async fn json_chat(&self, _system: &str, _user: &str) -> Result<Option<String>> {
            Ok(Some(self.0.to_string()))
        }
    }

    fn memory(id: &str, content: &str) -> Memory {
        let mut mem = Memory::new(
            MemoryType::Preference,
            content.to_string(),
            Priority::Must,
            SourceAgent {
                id: "codex".into(),
                agent_type: "cli".into(),
                session_id: None,
            },
        );
        mem.id = id.to_string();
        mem.human_reviewed = true;
        mem
    }

    fn db_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "memvault-compliance-judge-{tag}-{}.db",
            uuid::Uuid::new_v4()
        ))
    }

    #[tokio::test]
    async fn finalize_without_judge_records_unknown() {
        let path = db_path("unknown");
        let store = Arc::new(SqliteStore::new(Path::new(&path)).unwrap());
        let compliance = ComplianceStore::new(path.to_str().unwrap()).unwrap();
        store
            .save(memory("mem_a", "Always reply in Chinese."))
            .await
            .unwrap();
        compliance
            .record_injection_with_host_session(
                "inj_1",
                "mem_a",
                &Priority::Must,
                "codex",
                Some("host-1"),
            )
            .await
            .unwrap();

        let report = finalize_host_session(&compliance, store.as_ref(), None, "host-1", "")
            .await
            .unwrap();
        assert_eq!(report.unknown, 1);
        assert_eq!(report.judged, 0);
        assert!(
            compliance
                .pending_compliance_for_host_session("host-1")
                .await
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn finalize_with_judge_records_followed() {
        let path = db_path("followed");
        let store = Arc::new(SqliteStore::new(Path::new(&path)).unwrap());
        let compliance = ComplianceStore::new(path.to_str().unwrap()).unwrap();
        store
            .save(memory("mem_a", "Always reply in Chinese."))
            .await
            .unwrap();
        compliance
            .record_injection_with_host_session(
                "inj_1",
                "mem_a",
                &Priority::Must,
                "codex",
                Some("host-1"),
            )
            .await
            .unwrap();
        let judge = FixedJudge(
            r#"{"status":"followed","evidence":"assistant replied in Chinese","reason":"ok"}"#,
        );

        let report = finalize_host_session(
            &compliance,
            store.as_ref(),
            Some(&judge),
            "host-1",
            "user: 你好\nassistant: 你好，有什么可以帮你？",
        )
        .await
        .unwrap();
        assert_eq!(report.judged, 1);
        let report = compliance.get_report("inj_1").await.unwrap();
        assert_eq!(report.must_followed, 1);
        assert_eq!(report.compliance_rate, Some(1.0));
        let _ = std::fs::remove_file(path);
    }
}
