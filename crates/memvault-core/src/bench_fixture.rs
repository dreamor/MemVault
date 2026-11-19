//! Versioned, isolated task-benchmark fixtures; never opens a user's vault.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::bench::{BenchConfig, BenchReport, BenchSample, run_bench_with_project};
use crate::error::{MemVaultError, Result};
use crate::llm_extractor::LlmExtractor;
use crate::models::{AgentProfile, EpisodeRecord, Memory, MemoryType};
use crate::router::MemoryRouter;
use crate::storage::MemoryStore;
use crate::storage::sqlite::SqliteStore;

const FIXTURE_VERSION: u32 = 1;
const MAX_FIXTURE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchMode {
    Production,
    Diagnostic,
}

impl BenchMode {
    pub fn include_unreviewed(self) -> bool {
        self == Self::Diagnostic
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchFixture {
    pub schema_version: u32,
    pub fixture_id: String,
    pub memories: Vec<Memory>,
    pub episodes: Vec<EpisodeRecord>,
    pub sample_episode_ids: Vec<String>,
    #[serde(default)]
    pub agents: Vec<AgentProfile>,
    #[serde(default)]
    pub project: Option<String>,
}

pub struct LoadedBenchFixture {
    pub fixture: BenchFixture,
    /// Hash of the exact bytes that were parsed, not a reserialization hash.
    pub source_sha256: String,
}

fn invalid(message: impl Into<String>) -> MemVaultError {
    MemVaultError::InvalidInput(message.into())
}

fn reject_credentials(value: &serde_json::Value) -> Result<()> {
    match value {
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                let name = key.to_ascii_lowercase().replace(['-', '_'], "");
                if matches!(
                    name.as_str(),
                    "apikey"
                        | "authorization"
                        | "password"
                        | "secret"
                        | "secretkey"
                        | "clientsecret"
                        | "apitoken"
                        | "bearertoken"
                        | "credentials"
                        | "accesstoken"
                        | "refreshtoken"
                ) && !value.is_null()
                {
                    return Err(invalid("benchmark fixtures must not contain credentials"));
                }
                reject_credentials(value)?;
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                reject_credentials(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

impl LoadedBenchFixture {
    pub fn from_path(path: &Path) -> Result<Self> {
        let file =
            std::fs::File::open(path).map_err(|error| MemVaultError::Storage(error.to_string()))?;
        let mut bytes = Vec::new();
        file.take((MAX_FIXTURE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| MemVaultError::Storage(error.to_string()))?;
        Self::from_bytes(&bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_FIXTURE_BYTES {
            return Err(invalid("benchmark fixture exceeds the 16 MiB limit"));
        }
        let value: serde_json::Value = serde_json::from_slice(bytes)?;
        reject_credentials(&value)?;
        let fixture: BenchFixture = serde_json::from_slice(bytes)?;
        fixture.validate()?;
        Ok(Self {
            fixture,
            source_sha256: hex::encode(Sha256::digest(bytes)),
        })
    }
}

impl BenchFixture {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != FIXTURE_VERSION || self.fixture_id.trim().is_empty() {
            return Err(invalid(
                "benchmark fixture needs schema_version=1 and a nonempty fixture_id",
            ));
        }
        let mut memories = HashMap::new();
        for memory in &self.memories {
            if memory.id.trim().is_empty() || memories.insert(memory.id.as_str(), memory).is_some()
            {
                return Err(invalid(
                    "benchmark fixture contains empty or duplicate memory IDs",
                ));
            }
            if memory.fact_management.is_some() {
                return Err(invalid(
                    "managed fact snapshots are not supported by benchmark fixtures",
                ));
            }
        }
        let mut episodes = HashSet::new();
        for episode in &self.episodes {
            if !episodes.insert(episode.memory_id.as_str()) {
                return Err(invalid("benchmark fixture contains duplicate episode IDs"));
            }
            let parent = memories
                .get(episode.memory_id.as_str())
                .ok_or_else(|| invalid("every fixture episode must have its parent memory"))?;
            if parent.memory_type != MemoryType::Episode {
                return Err(invalid("fixture episode parent must have type=episode"));
            }
        }
        if self.sample_episode_ids.is_empty() {
            return Err(invalid(
                "benchmark fixture must select at least one episode",
            ));
        }
        let mut selected = HashSet::new();
        for id in &self.sample_episode_ids {
            if !episodes.contains(id.as_str()) || !selected.insert(id.as_str()) {
                return Err(invalid(
                    "selected fixture episode IDs must be present and unique",
                ));
            }
        }
        let mut agents = HashSet::new();
        for agent in &self.agents {
            if agent.id.trim().is_empty() || !agents.insert(agent.id.as_str()) {
                return Err(invalid("fixture agent IDs must be nonempty and unique"));
            }
            if agent.api_key.is_some() {
                return Err(invalid("fixture agent profiles must not have API keys"));
            }
            if agent.inject_rules.max_memories.checked_mul(2).is_none() {
                return Err(invalid(
                    "fixture agent max_memories exceeds the selector arithmetic range",
                ));
            }
        }
        if self.project.as_ref().is_some_and(|p| p.trim().is_empty()) {
            return Err(invalid("fixture project must be absent or nonempty"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixtureModeRun {
    pub mode: BenchMode,
    pub config_sha256: String,
    pub report: BenchReport,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FixtureTransitionCounts {
    pub both: usize,
    pub neither: usize,
    pub gained: usize,
    pub lost: usize,
}

impl FixtureTransitionCounts {
    fn record(&mut self, before: bool, after: bool) {
        match (before, after) {
            (true, true) => self.both += 1,
            (false, false) => self.neither += 1,
            (false, true) => self.gained += 1,
            (true, false) => self.lost += 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixtureComparison {
    pub before_mode: BenchMode,
    pub after_mode: BenchMode,
    pub samples: usize,
    pub retrieval: FixtureTransitionCounts,
    pub injection_selection: FixtureTransitionCounts,
    pub delivery: FixtureTransitionCounts,
    pub meaning: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixtureBenchReport {
    pub schema_version: u32,
    pub fixture_id: String,
    pub fixture_sha256: String,
    pub input_sample_count: usize,
    pub sample_episode_ids: Vec<String>,
    pub started_at: String,
    pub storage: String,
    pub history_persisted: bool,
    pub embeddings_enabled: bool,
    pub judging_requested: bool,
    pub clock_note: String,
    pub runs: Vec<FixtureModeRun>,
    pub comparison: Option<FixtureComparison>,
}

fn configuration_fingerprint(
    config: &BenchConfig,
    report: &BenchReport,
    selected: &[String],
) -> Result<String> {
    let value = serde_json::json!({
        "requested": config,
        "effective": report.effective_config,
        "scoring_mode": report.mode,
        "sample_episode_ids": selected,
    });
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(&value)?)))
}

/// Rebuild identical in-memory state for each mode; no user configuration discovery.
pub async fn run_fixture_bench(
    loaded: &LoadedBenchFixture,
    config: &BenchConfig,
    sample_limit: usize,
    modes: &[BenchMode],
    judge: Option<&Arc<dyn LlmExtractor>>,
) -> Result<FixtureBenchReport> {
    loaded.fixture.validate()?;
    if sample_limit == 0 || modes.is_empty() || modes.len() > 2 {
        return Err(invalid(
            "fixture runs need a positive sample limit and one or two modes",
        ));
    }
    if modes.len() == 2 && modes[0] == modes[1] {
        return Err(invalid("fixture comparison modes must differ"));
    }
    let fixture = &loaded.fixture;
    let selected: Vec<String> = fixture
        .sample_episode_ids
        .iter()
        .take(sample_limit)
        .cloned()
        .collect();
    let episodes: HashMap<&str, &EpisodeRecord> = fixture
        .episodes
        .iter()
        .map(|e| (e.memory_id.as_str(), e))
        .collect();
    let mut output = FixtureBenchReport {
        schema_version: 1, fixture_id: fixture.fixture_id.clone(),
        fixture_sha256: loaded.source_sha256.clone(), input_sample_count: fixture.sample_episode_ids.len(),
        sample_episode_ids: selected.clone(), started_at: chrono::Utc::now().to_rfc3339(),
        storage: "fresh_in_memory_sqlite_per_mode".to_string(), history_persisted: false,
        embeddings_enabled: false, judging_requested: judge.is_some(),
        clock_note: "Inputs are frozen; router recency/lifecycle selection uses the recorded run's wall clock.".to_string(),
        runs: Vec::new(), comparison: None,
    };
    for &mode in modes {
        let store = Arc::new(SqliteStore::in_memory()?);
        for memory in &fixture.memories {
            let saved = store.save(memory.clone()).await?;
            if saved.id != memory.id {
                return Err(invalid("fixture memory identity changed during seeding"));
            }
        }
        for episode in &fixture.episodes {
            store.record_episode(episode.clone()).await?;
        }
        let mut samples = Vec::new();
        for id in &selected {
            let episode = (**episodes
                .get(id.as_str())
                .ok_or_else(|| invalid("selected episode disappeared"))?)
            .clone();
            let lesson_memory = match &episode.lesson_memory_id {
                Some(target) => match store.get(target).await {
                    Ok(memory) => Some(memory),
                    Err(MemVaultError::NotFound(_)) => None,
                    Err(error) => return Err(error),
                },
                None => None,
            };
            samples.push(BenchSample {
                episode,
                lesson_memory,
            });
        }
        let router = MemoryRouter::with_registry(store.clone(), fixture.agents.clone());
        let mut run_config = config.clone();
        run_config.include_unreviewed = mode.include_unreviewed();
        let report = run_bench_with_project(
            &router,
            store.as_ref(),
            &samples,
            &run_config,
            judge,
            fixture.project.as_deref(),
        )
        .await?;
        let config_sha256 = configuration_fingerprint(&run_config, &report, &selected)?;
        output.runs.push(FixtureModeRun {
            mode,
            config_sha256,
            report,
        });
    }
    if output.runs.len() == 2 {
        let before = &output.runs[0];
        let after = &output.runs[1];
        if before.report.rows.len() != selected.len() || after.report.rows.len() != selected.len() {
            return Err(invalid(
                "fixture comparison rows do not match selected episode order",
            ));
        }
        let mut comparison = FixtureComparison {
            before_mode: before.mode,
            after_mode: after.mode,
            samples: selected.len(),
            retrieval: FixtureTransitionCounts::default(),
            injection_selection: FixtureTransitionCounts::default(),
            delivery: FixtureTransitionCounts::default(),
            meaning:
                "Reported ID retrieval/selection transitions only; not task execution success."
                    .to_string(),
        };
        for (a, b) in before.report.rows.iter().zip(&after.report.rows) {
            comparison.retrieval.record(a.retrieved, b.retrieved);
            comparison
                .injection_selection
                .record(a.injected, b.injected);
            comparison.delivery.record(
                a.lesson_memory_id
                    .as_ref()
                    .is_some_and(|id| a.delivered_ids.contains(id)),
                b.lesson_memory_id
                    .as_ref()
                    .is_some_and(|id| b.delivered_ids.contains(id)),
            );
        }
        output.comparison = Some(comparison);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{OutcomeStatus, Priority, SourceAgent};

    fn fixture() -> BenchFixture {
        let source = SourceAgent {
            id: "fixture".to_string(),
            agent_type: "test".to_string(),
            session_id: None,
        };
        let mut parent = Memory::new(
            MemoryType::Episode,
            "fixture episode".to_string(),
            Priority::Reference,
            source.clone(),
        );
        parent.id = "fixture-episode".to_string();
        let mut lesson = Memory::new(
            MemoryType::Fact,
            "Before fixture tasks, set FEATURE_MODE to strict".to_string(),
            Priority::Reference,
            source,
        );
        lesson.id = "fixture-lesson".to_string();
        lesson.instruction = Some(lesson.content.clone());
        BenchFixture {
            schema_version: 1,
            fixture_id: "fixture-v1".to_string(),
            memories: vec![parent, lesson],
            episodes: vec![EpisodeRecord {
                memory_id: "fixture-episode".to_string(),
                task: "Run the fixture task".to_string(),
                task_type: Some("fixture".to_string()),
                status: OutcomeStatus::Failure,
                cause: Some("FEATURE_MODE was missing".to_string()),
                lesson: Some("Set FEATURE_MODE to strict".to_string()),
                lesson_memory_id: Some("fixture-lesson".to_string()),
                outcome_provenance: None,
                occurred_at: chrono::Utc::now(),
            }],
            sample_episode_ids: vec!["fixture-episode".to_string()],
            agents: Vec::new(),
            project: None,
        }
    }

    fn loaded(f: &BenchFixture) -> LoadedBenchFixture {
        LoadedBenchFixture::from_bytes(&serde_json::to_vec(f).unwrap()).unwrap()
    }

    #[test]
    fn bytes_and_order_are_validated_without_a_vault() {
        let f = fixture();
        let bytes = serde_json::to_vec(&f).unwrap();
        let parsed = LoadedBenchFixture::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.source_sha256, hex::encode(Sha256::digest(&bytes)));
        assert_eq!(parsed.fixture.sample_episode_ids, f.sample_episode_ids);
    }

    #[test]
    fn rejects_unknown_version_duplicate_ids_and_missing_parent() {
        let mut f = fixture();
        f.schema_version = 2;
        assert!(f.validate().is_err());
        f = fixture();
        f.memories.push(f.memories[0].clone());
        assert!(f.validate().is_err());
        f = fixture();
        f.memories.remove(0);
        assert!(f.validate().is_err());
        f = fixture();
        f.sample_episode_ids.push(f.sample_episode_ids[0].clone());
        assert!(f.validate().is_err());
    }

    #[test]
    fn credentials_are_rejected_and_missing_lesson_is_a_valid_negative() {
        let mut value = serde_json::to_value(fixture()).unwrap();
        value["agents"] =
            serde_json::json!([{"id":"default","agent_type":"test","api_key":"do-not-copy"}]);
        assert!(LoadedBenchFixture::from_bytes(&serde_json::to_vec(&value).unwrap()).is_err());
        let mut f = fixture();
        f.memories.pop();
        assert!(f.validate().is_ok());
    }

    #[test]
    fn excessive_profile_counts_and_nested_secrets_are_rejected() {
        let mut f = fixture();
        f.agents.push(AgentProfile {
            id: "fixture".to_string(),
            agent_type: "test".to_string(),
            description: String::new(),
            inject_rules: crate::models::InjectRules {
                max_memories: usize::MAX,
                ..Default::default()
            },
            api_key: None,
            inject_channel: None,
        });
        assert!(f.validate().is_err());
        assert!(LoadedBenchFixture::from_bytes(&serde_json::to_vec(&f).unwrap()).is_err());
        f.agents[0].inject_rules.max_memories = 8;
        assert!(f.validate().is_ok());
        for name in [
            "refresh_token",
            "client_secret",
            "secret_key",
            "bearer_token",
            "refreshToken",
            "client-secret",
            "SecretKey",
            "bearerToken",
        ] {
            let mut value = serde_json::to_value(&f).unwrap();
            value["memories"][0][name] = serde_json::json!("do-not-copy");
            assert!(LoadedBenchFixture::from_bytes(&serde_json::to_vec(&value).unwrap()).is_err());
        }
    }

    #[tokio::test]
    async fn fingerprint_includes_effective_gates_profile_and_scoring_mode() {
        let f = fixture();
        let config = BenchConfig {
            include_unreviewed: false,
            ..BenchConfig::default()
        };
        let output = run_fixture_bench(&loaded(&f), &config, 1, &[BenchMode::Production], None)
            .await
            .unwrap();
        let report = &output.runs[0].report;
        let hash = configuration_fingerprint(&config, report, &f.sample_episode_ids).unwrap();
        assert_eq!(hash, output.runs[0].config_sha256);
        let mut changed = report.clone();
        let effective = changed.effective_config.as_mut().unwrap();
        effective.require_review = !effective.require_review;
        assert_ne!(
            hash,
            configuration_fingerprint(&config, &changed, &f.sample_episode_ids).unwrap()
        );
        changed = report.clone();
        let effective = changed.effective_config.as_mut().unwrap();
        effective.strict_must = !effective.strict_must;
        assert_ne!(
            hash,
            configuration_fingerprint(&config, &changed, &f.sample_episode_ids).unwrap()
        );
        changed = report.clone();
        let effective = changed.effective_config.as_mut().unwrap();
        effective.corroboration_gate = !effective.corroboration_gate;
        assert_ne!(
            hash,
            configuration_fingerprint(&config, &changed, &f.sample_episode_ids).unwrap()
        );
        changed = report.clone();
        changed.effective_config.as_mut().unwrap().effective_top_k += 1;
        assert_ne!(
            hash,
            configuration_fingerprint(&config, &changed, &f.sample_episode_ids).unwrap()
        );
        changed = report.clone();
        changed.mode = crate::bench::BenchMode::NativeFourStage;
        assert_ne!(
            hash,
            configuration_fingerprint(&config, &changed, &f.sample_episode_ids).unwrap()
        );
    }

    #[tokio::test]
    async fn each_mode_has_fresh_state_and_unknown_task_success_without_judge() {
        let f = fixture();
        let output = run_fixture_bench(
            &loaded(&f),
            &BenchConfig::default(),
            20,
            &[BenchMode::Production, BenchMode::Diagnostic],
            None,
        )
        .await
        .unwrap();
        assert_eq!(output.runs.len(), 2);
        assert_eq!(output.runs[0].report.judged, 0);
        assert_eq!(output.runs[1].report.judged, 0);
        assert!(!output.history_persisted);
        assert!(!output.embeddings_enabled);
        assert_ne!(output.runs[0].config_sha256, output.runs[1].config_sha256);
        assert_eq!(output.comparison.as_ref().unwrap().samples, 1);
    }

    #[tokio::test]
    async fn missing_target_does_not_disappear_from_selection() {
        let mut f = fixture();
        f.memories.pop();
        let output = run_fixture_bench(
            &loaded(&f),
            &BenchConfig::default(),
            1,
            &[BenchMode::Diagnostic],
            None,
        )
        .await
        .unwrap();
        assert_eq!(output.runs[0].report.total, 1);
        assert!(!output.runs[0].report.rows[0].retrieved);
        assert!(!output.runs[0].report.rows[0].injected);
    }

    #[tokio::test]
    async fn invalid_limits_and_duplicate_modes_fail_before_any_run() {
        let f = loaded(&fixture());
        assert!(
            run_fixture_bench(
                &f,
                &BenchConfig::default(),
                0,
                &[BenchMode::Diagnostic],
                None
            )
            .await
            .is_err()
        );
        assert!(
            run_fixture_bench(
                &f,
                &BenchConfig::default(),
                1,
                &[BenchMode::Diagnostic, BenchMode::Diagnostic],
                None
            )
            .await
            .is_err()
        );
    }
}
