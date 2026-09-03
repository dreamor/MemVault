use crate::auth::hash_key;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryType {
    Preference,
    Fact,
    Episode,
    Entity,
    Skill,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum Priority {
    Must,
    Reference,
    Background,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceAgent {
    pub id: String,
    pub agent_type: String,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    #[serde(rename = "type")]
    pub memory_type: MemoryType,
    pub content: String,
    pub instruction: Option<String>,
    pub priority: Priority,
    pub source_agent: SourceAgent,
    pub namespace: String,
    pub confidence: f64,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub ai_generated: bool,
    pub human_reviewed: bool,
    pub decay_score: f64,
    pub access_count: u32,
    pub last_read_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub layer: MemoryLayer,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_meta: Option<SkillMeta>,
    /// Sharing scope (Phase D). Default `Scoped` = existing namespace rules;
    /// `Shared` = team pool, additionally injected into every session.
    #[serde(default)]
    pub visibility: Visibility,
    /// Set when a newer fact supersedes this one (semantic versioning).
    /// Superseded memories are archived (layer L0), never deleted, so the
    /// history stays restorable; retrieval skips them by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
}

impl Memory {
    pub fn new(
        memory_type: MemoryType,
        content: String,
        priority: Priority,
        source_agent: SourceAgent,
    ) -> Self {
        let now = Utc::now();
        let layer = match priority {
            Priority::Must => MemoryLayer::L3,
            Priority::Reference => MemoryLayer::L2,
            Priority::Background => MemoryLayer::L1,
        };
        Self {
            id: format!("mem_{}", uuid::Uuid::new_v4().as_simple()),
            memory_type,
            content,
            instruction: None,
            priority,
            source_agent,
            namespace: "global".to_string(),
            confidence: 0.8,
            tags: Vec::new(),
            created_at: now,
            updated_at: now,
            ai_generated: true,
            human_reviewed: false,
            decay_score: 1.0,
            access_count: 0,
            last_read_at: None,
            layer,
            skill_meta: None,
            visibility: Visibility::Scoped,
            superseded_by: None,
        }
    }
}

/// Outcome of a task an agent executed. Episodic memories attach one of
/// these so retrieval can filter "past failures at X" instead of grepping
/// free text.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeStatus {
    Success,
    Failure,
    Partial,
}

impl OutcomeStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            OutcomeStatus::Success => "success",
            OutcomeStatus::Failure => "failure",
            OutcomeStatus::Partial => "partial",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "success" => Some(Self::Success),
            "failure" | "failed" => Some(Self::Failure),
            "partial" => Some(Self::Partial),
            _ => None,
        }
    }
}

/// Structured result of one task execution, linked 1:1 to an episode
/// `Memory` row. The memory row carries content/tags/namespace (and stays
/// keyword+vector searchable); this record carries the outcome semantics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeRecord {
    pub memory_id: String,
    /// What the agent was trying to do.
    pub task: String,
    /// Coarse task category for intent-style matching (deploy/debug/...).
    pub task_type: Option<String>,
    pub status: OutcomeStatus,
    /// Attribution of the outcome, when known.
    pub cause: Option<String>,
    /// Lesson distilled by reflection (filled after the fact).
    pub lesson: Option<String>,
    /// The instruction-form memory generated from `lesson`, if any.
    pub lesson_memory_id: Option<String>,
    pub occurred_at: DateTime<Utc>,
}

/// Filters for listing episode records. All fields optional; `limit` bounds
/// the result set (newest first).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeFilter {
    pub task_type: Option<String>,
    pub status: Option<OutcomeStatus>,
    pub namespace: Option<String>,
    #[serde(default = "default_episode_limit")]
    pub limit: usize,
}

fn default_episode_limit() -> usize {
    50
}

impl Default for EpisodeFilter {
    fn default() -> Self {
        Self {
            task_type: None,
            status: None,
            namespace: None,
            limit: default_episode_limit(),
        }
    }
}

/// Which delivery channel is the canonical (only) automatic injection path
/// for an agent — Feature F (docs/PAPER-INSPIRATIONS.md, paper Table 7:
/// spreading one memory budget across multiple injection layers yields no
/// benefit, only duplication). Unset (`None`) means "no restriction": every
/// channel may inject, which preserves pre-Feature-F behavior. Setting it
/// dedups injection so a single agent does not receive the same memory from
/// MCP session_start, the transparent proxy, and synced instruction files all
/// at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InjectChannel {
    /// Explicit `session_start` (MCP tool / REST `/api/session`).
    Mcp,
    /// Transparent proxy auto-injection.
    Proxy,
    /// Generated instruction files (`memvault sync`).
    Sync,
}

impl InjectChannel {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "mcp" | "session" | "session_start" => Some(Self::Mcp),
            "proxy" => Some(Self::Proxy),
            "sync" | "file" | "files" => Some(Self::Sync),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            InjectChannel::Mcp => "mcp",
            InjectChannel::Proxy => "proxy",
            InjectChannel::Sync => "sync",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProfile {
    pub id: String,
    pub agent_type: String,
    pub description: String,
    pub inject_rules: InjectRules,
    /// Optional API key for agent authentication.
    /// When set, the client must provide matching credentials on tool calls.
    /// The key is hashed (SHA-256) on load and never stored in plaintext.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Canonical injection channel (Feature F). `None` (default) = every
    /// channel may inject (backward compatible). Set to restrict the agent to
    /// a single delivery path and avoid duplicate injection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inject_channel: Option<InjectChannel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InjectRules {
    pub max_memories: usize,
    pub token_budget: usize,
    pub priority_order: Vec<Priority>,
    pub namespace_filter: Vec<String>,
    pub exclude_types: Vec<String>,
}

impl Default for InjectRules {
    fn default() -> Self {
        Self {
            max_memories: 8,
            token_budget: 1500,
            priority_order: vec![Priority::Must, Priority::Reference],
            namespace_filter: vec!["global".to_string()],
            exclude_types: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQuery {
    pub query: String,
    pub agent_id: Option<String>,
    pub type_filter: Option<MemoryType>,
    pub priority_filter: Option<Priority>,
    pub namespace: Option<String>,
    pub top_k: usize,
    pub token_budget: Option<usize>,
    /// Attach one-hop relations to each result (semantic graph expansion,
    /// C5). Default off — callers opt in when they want the graph context.
    #[serde(default)]
    pub expand_relations: bool,
}

impl SearchQuery {
    pub fn new(query: String) -> Self {
        Self {
            query,
            agent_id: None,
            type_filter: None,
            priority_filter: None,
            namespace: None,
            top_k: 10,
            token_budget: None,
            expand_relations: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub memory: Memory,
    pub score: f64,
    /// Recall provenance: which retrieval path(s) surfaced this memory and at
    /// what rank in each. Answers "why is this ranked first?" without
    /// guessing, and lets injection auditing cite how a memory was recalled.
    /// Empty for results that never passed through a ranked retrieval path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hit_sources: Vec<HitSource>,
}

/// One retrieval path that recalled a result, with the 1-based rank the
/// result held in that path's own ranked list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HitSource {
    Keyword {
        rank: usize,
    },
    Vector {
        rank: usize,
    },
    /// Recalled by explicit episodic/procedural matching (lesson task_type /
    /// skill trigger against the session context) rather than ranked
    /// retrieval. Quota enforcement reserves slots for these first, so a
    /// matched procedure is never bumped by generic-search floats.
    ExplicitMatch,
}

impl HitSource {
    /// Short display tag, e.g. `kw#2` / `vec#5`, for CLI/MCP annotations.
    pub fn tag(&self) -> String {
        match self {
            HitSource::Keyword { rank } => format!("kw#{rank}"),
            HitSource::Vector { rank } => format!("vec#{rank}"),
            HitSource::ExplicitMatch => "match".to_string(),
        }
    }
}

/// Which keyword-match tier produced a search outcome. Anything other than
/// [`KeywordTier::Strict`] / [`KeywordTier::None`] means matching was relaxed
/// and precision is reduced — that MUST be reported to callers, never hidden:
/// silently relaxed results would be mistaken for exact matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum KeywordTier {
    /// No keyword constraint was applied (empty query).
    #[default]
    None,
    /// Full tokenization matched (CJK unigrams + bigrams, AND-combined).
    Strict,
    /// Strict tier returned nothing; fell back to CJK unigrams only.
    RelaxedUnigram,
    /// Both strict tiers returned nothing; fell back to an OR over query
    /// tokens and synonym-expansion tokens.
    SynonymFallback,
}

/// Result of a keyword search: the ranked rows plus which tier matched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchOutcome {
    pub results: Vec<SearchResult>,
    pub keyword_tier: KeywordTier,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
#[derive(Default)]
pub enum MemoryLayer {
    L0,
    #[default]
    L1,
    L2,
    L3,
}

/// Sharing scope of a memory (Phase D team shared pool).
/// - `Scoped` (default): injected only per the existing namespace rules.
/// - `Shared`: team-pool knowledge — additionally injected into every session
///   regardless of namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    #[default]
    Scoped,
    Shared,
}

impl Visibility {
    pub fn as_str(&self) -> &'static str {
        match self {
            Visibility::Scoped => "scoped",
            Visibility::Shared => "shared",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "shared" => Visibility::Shared,
            _ => Visibility::Scoped,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillMeta {
    pub trigger: Option<String>,
    pub steps: Vec<String>,
    pub verification: Option<String>,
    #[serde(default = "default_skill_version")]
    pub version: u32,
}

/// Tag marking a skill whose associated task failed — the procedure likely
/// needs a human revision. Applied automatically when a recorded failure's
/// task_type matches the skill trigger; cleared when the skill is edited
/// (which also bumps `version`).
pub const NEEDS_REVISION_TAG: &str = "needs-revision";

/// Tag marking an auto-drafted skill distilled from repeated successes.
/// Drafts always enter the review queue — distillation proposes, humans
/// dispose.
pub const SKILL_DRAFT_TAG: &str = "skill-draft";

/// Number of same-type successes that trigger a skill draft proposal.
pub const SKILL_DRAFT_THRESHOLD: usize = 3;

/// Minimum number of recorded executions before a skill's success rate is
/// shown. Below this the sample is too small to be meaningful — displaying
/// "100% (1 run)" would mislead agents into over-trusting an unproven skill.
pub const SKILL_RATE_MIN_SAMPLES: usize = 3;

/// Aggregated execution statistics for one skill memory, driving the
/// success-rate display in skill injection and the Dashboard.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillStats {
    pub skill_memory_id: String,
    /// How many sessions this skill was injected into.
    pub injected_count: u32,
    /// Attributed task successes (record_outcome with this skill_id).
    pub success_count: u32,
    /// Attributed task failures.
    pub failure_count: u32,
}

/// One directed relation between memories (semantic graph edge, stored as a
/// lightweight triple — no graph database). `subject_id` is always a memory;
/// the object is either another memory (`object_id`) or free text
/// (`object_text`), exactly one of the two. `source_memory_id` records the
/// episode/extraction the relation was distilled from (provenance).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRelation {
    /// Database id; `None` before the row is inserted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relation_id: Option<i64>,
    pub subject_id: String,
    /// uses | belongs_to | depends_on | decided | located_in | ...
    pub predicate: String,
    pub object_id: Option<String>,
    pub object_text: Option<String>,
    pub confidence: f64,
    pub source_memory_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl SkillStats {
    pub fn executions(&self) -> u32 {
        self.success_count + self.failure_count
    }

    /// Success rate in [0,1], or `None` when the sample is below
    /// [`SKILL_RATE_MIN_SAMPLES`] — callers must not display a rate then.
    pub fn success_rate(&self) -> Option<f64> {
        let total = self.executions();
        if (total as usize) < SKILL_RATE_MIN_SAMPLES {
            return None;
        }
        Some(self.success_count as f64 / total as f64)
    }
}

fn default_skill_version() -> u32 {
    1
}

impl Default for SkillMeta {
    fn default() -> Self {
        Self {
            trigger: None,
            steps: Vec::new(),
            verification: None,
            version: 1,
        }
    }
}

/// Why a memory candidate was NOT injected. Silent drops are the hardest
/// injection problem to debug ("I saved it, why didn't the agent get it?"),
/// so every stage that removes a candidate must name a reason — a new drop
/// stage that forgets one fails to compile against this closed enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InjectSkipReason {
    /// Penalized by the agent's exclude_types rules until its score fell
    /// below the floor.
    TypeExcluded,
    /// Penalized by intent mismatch until its score fell below the floor.
    IntentFiltered,
    /// Score below the injection floor (and no exclusion penalty applies).
    BelowScoreFloor,
    /// Cut by the token budget.
    TokenBudgetExceeded,
    /// Cut by the max-memories cap after the budget trim.
    MaxMemoriesExceeded,
    /// Cut by the per-session lesson quota — a long failure history must not
    /// crowd out the working context (MUST lessons are exempt).
    LessonQuotaExceeded,
    /// Cut by the per-session skill quota — at most a couple of procedures
    /// per session, so steps don't drown the working context.
    SkillQuotaExceeded,
}

impl std::fmt::Display for InjectSkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            InjectSkipReason::TypeExcluded => "type-excluded",
            InjectSkipReason::IntentFiltered => "intent-filtered",
            InjectSkipReason::BelowScoreFloor => "below-score-floor",
            InjectSkipReason::TokenBudgetExceeded => "token-budget-exceeded",
            InjectSkipReason::MaxMemoriesExceeded => "max-memories-exceeded",
            InjectSkipReason::LessonQuotaExceeded => "lesson-quota-exceeded",
            InjectSkipReason::SkillQuotaExceeded => "skill-quota-exceeded",
        };
        write!(f, "{s}")
    }
}

/// One dropped candidate plus the reason it was dropped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedMemory {
    pub id: String,
    pub reason: InjectSkipReason,
}

/// A pair of currently-injected memories with an active `contradicts`
/// relation between them. Surfaced so the agent flags the conflict and lets
/// the human decide instead of silently picking a winner — decay already
/// forgets contradicted memories faster over time, but that's slow and
/// invisible; this is the same-session, visible counterpart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictNotice {
    pub memory_id: String,
    pub conflicting_with: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStartOutput {
    pub injected: Vec<SearchResult>,
    pub overflow_count: usize,
    pub overflow_summaries: Vec<String>,
    /// Candidates dropped on the way (with reasons). Empty in the common case
    /// where nothing was filtered; serde-default keeps older payloads valid.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<SkippedMemory>,
    /// Contradicting pairs found among `injected` (see [`ConflictNotice`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<ConflictNotice>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRegistryConfig {
    pub agents: Vec<AgentProfile>,
}

impl AgentRegistryConfig {
    /// Hash all api_key values in the agent profiles for secure in-memory storage.
    /// Call this after deserializing from YAML to avoid keeping plaintext keys.
    pub fn hash_api_keys_in_place(&mut self) {
        for agent in &mut self.agents {
            if let Some(ref key) = agent.api_key.take()
                && !key.is_empty()
            {
                // Store the hex-encoded SHA-256 hash instead of the raw key
                agent.api_key = Some(hash_key(key));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_memory_new_defaults() {
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };
        let mem = Memory::new(
            MemoryType::Fact,
            "test content".to_string(),
            Priority::Reference,
            agent,
        );

        assert!(mem.id.starts_with("mem_"));
        assert_eq!(mem.memory_type, MemoryType::Fact);
        assert_eq!(mem.content, "test content");
        assert_eq!(mem.priority, Priority::Reference);
        assert_eq!(mem.namespace, "global");
        assert_eq!(mem.confidence, 0.8);
        assert!(mem.tags.is_empty());
        assert!(mem.instruction.is_none());
        assert!(mem.ai_generated);
        assert!(!mem.human_reviewed);
        assert_eq!(mem.decay_score, 1.0);
        assert_eq!(mem.access_count, 0);
        assert!(mem.last_read_at.is_none());
    }

    #[test]
    fn test_memory_new_must_priority() {
        let agent = SourceAgent {
            id: "claude".to_string(),
            agent_type: "assistant".to_string(),
            session_id: Some("sess_1".to_string()),
        };
        let mem = Memory::new(
            MemoryType::Preference,
            "must rule".to_string(),
            Priority::Must,
            agent,
        );

        assert_eq!(mem.priority, Priority::Must);
    }

    #[test]
    fn test_memory_new_timestamps() {
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "tester".to_string(),
            session_id: None,
        };
        let before = Utc::now();
        let mem = Memory::new(
            MemoryType::Episode,
            "event".to_string(),
            Priority::Background,
            agent,
        );
        let after = Utc::now();

        assert!(mem.created_at >= before);
        assert!(mem.created_at <= after);
        assert_eq!(mem.created_at, mem.updated_at);
    }

    #[test]
    fn test_search_query_new() {
        let q = SearchQuery::new("find me".to_string());

        assert_eq!(q.query, "find me");
        assert_eq!(q.top_k, 10);
        assert!(q.agent_id.is_none());
        assert!(q.type_filter.is_none());
        assert!(q.priority_filter.is_none());
        assert!(q.namespace.is_none());
        assert!(q.token_budget.is_none());
    }

    #[test]
    fn test_search_query_custom() {
        let q = SearchQuery {
            query: "search".to_string(),
            agent_id: Some("agent1".to_string()),
            type_filter: Some(MemoryType::Preference),
            priority_filter: Some(Priority::Must),
            namespace: Some("project-alpha".to_string()),
            top_k: 5,
            token_budget: Some(500),
            expand_relations: true,
        };

        assert_eq!(q.query, "search");
        assert_eq!(q.agent_id, Some("agent1".to_string()));
        assert_eq!(q.top_k, 5);
        assert!(q.expand_relations);
    }

    #[test]
    fn test_inject_rules_default() {
        let rules = InjectRules::default();

        assert_eq!(rules.max_memories, 8);
        assert_eq!(rules.token_budget, 1500);
        assert_eq!(
            rules.priority_order,
            vec![Priority::Must, Priority::Reference]
        );
        assert_eq!(rules.namespace_filter, vec!["global".to_string()]);
        assert!(rules.exclude_types.is_empty());
    }

    #[test]
    fn test_skill_stats_rate_threshold() {
        let mut stats = SkillStats {
            skill_memory_id: "mem_s".into(),
            injected_count: 5,
            success_count: 2,
            failure_count: 0,
        };
        assert_eq!(stats.executions(), 2);
        assert!(stats.success_rate().is_none(), "2 samples < minimum");

        stats.failure_count = 1; // 3 samples total
        let rate = stats.success_rate().expect("3 samples shows a rate");
        assert!((rate - 2.0 / 3.0).abs() < 1e-9);

        let empty = SkillStats::default();
        assert!(empty.success_rate().is_none());
    }

    #[test]
    fn test_memory_type_serde() {
        let cases = vec![
            (MemoryType::Preference, "\"preference\""),
            (MemoryType::Fact, "\"fact\""),
            (MemoryType::Episode, "\"episode\""),
            (MemoryType::Entity, "\"entity\""),
            (MemoryType::Skill, "\"skill\""),
        ];

        for (variant, expected) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(
                json, expected,
                "MemoryType::{:?} serializes to {}",
                variant, expected
            );
            let deserialized: MemoryType = serde_json::from_str(expected).unwrap();
            assert_eq!(deserialized, variant);
        }
    }

    #[test]
    fn test_priority_serde() {
        let cases = vec![
            (Priority::Must, "\"MUST\""),
            (Priority::Reference, "\"REFERENCE\""),
            (Priority::Background, "\"BACKGROUND\""),
        ];

        for (variant, expected) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(
                json, expected,
                "Priority::{:?} serializes to {}",
                variant, expected
            );
            let deserialized: Priority = serde_json::from_str(expected).unwrap();
            assert_eq!(deserialized, variant);
        }
    }

    #[test]
    fn test_memory_roundtrip_serde() {
        let agent = SourceAgent {
            id: "serde-test".to_string(),
            agent_type: "tester".to_string(),
            session_id: Some("sess_s".to_string()),
        };
        let mem = Memory::new(
            MemoryType::Skill,
            "test the JSON roundtrip".to_string(),
            Priority::Background,
            agent,
        );

        let json = serde_json::to_string(&mem).unwrap();
        let deserialized: Memory = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id, mem.id);
        assert_eq!(deserialized.memory_type, mem.memory_type);
        assert_eq!(deserialized.content, mem.content);
        assert_eq!(deserialized.priority, mem.priority);
        assert_eq!(deserialized.namespace, mem.namespace);
    }

    #[test]
    fn test_source_agent_serde() {
        let agent = SourceAgent {
            id: "agent-x".to_string(),
            agent_type: "writer".to_string(),
            session_id: Some("abc".to_string()),
        };

        let json = serde_json::to_string(&agent).unwrap();
        let deserialized: SourceAgent = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id, "agent-x");
        assert_eq!(deserialized.agent_type, "writer");
        assert_eq!(deserialized.session_id, Some("abc".to_string()));
    }

    #[test]
    fn test_search_result_holds_data() {
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "tester".to_string(),
            session_id: None,
        };
        let mem = Memory::new(
            MemoryType::Fact,
            "searchable memory".to_string(),
            Priority::Reference,
            agent,
        );
        let result = SearchResult {
            memory: mem.clone(),
            score: 0.85,
            hit_sources: Vec::new(),
        };

        assert_eq!(result.memory.id, mem.id);
        assert_eq!(result.score, 0.85);
    }

    #[test]
    fn test_skill_meta_serde_roundtrip() {
        let meta = SkillMeta {
            trigger: Some("database timeout".to_string()),
            steps: vec!["check network".to_string(), "check pool".to_string()],
            verification: Some("connection < 100ms".to_string()),
            version: 2,
        };
        let json = serde_json::to_string(&meta).unwrap();
        let deserialized: SkillMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.trigger, Some("database timeout".to_string()));
        assert_eq!(deserialized.steps.len(), 2);
        assert_eq!(deserialized.version, 2);
    }

    #[test]
    fn test_memory_with_skill_meta_roundtrip() {
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };
        let mut mem = Memory::new(
            MemoryType::Skill,
            "deploy process".to_string(),
            Priority::Must,
            agent,
        );
        mem.skill_meta = Some(SkillMeta {
            trigger: Some("deploy".to_string()),
            steps: vec!["build".to_string(), "test".to_string(), "push".to_string()],
            verification: Some("health check passes".to_string()),
            version: 1,
        });

        let json = serde_json::to_string(&mem).unwrap();
        let deserialized: Memory = serde_json::from_str(&json).unwrap();
        assert!(deserialized.skill_meta.is_some());
        let sm = deserialized.skill_meta.unwrap();
        assert_eq!(sm.steps.len(), 3);
        assert_eq!(sm.trigger, Some("deploy".to_string()));
    }

    #[test]
    fn test_memory_without_skill_meta_compat() {
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };
        let mem = Memory::new(
            MemoryType::Fact,
            "plain fact".to_string(),
            Priority::Reference,
            agent,
        );
        let json = serde_json::to_string(&mem).unwrap();
        assert!(!json.contains("skill_meta"));
        let deserialized: Memory = serde_json::from_str(&json).unwrap();
        assert!(deserialized.skill_meta.is_none());
    }

    #[test]
    fn test_outcome_status_parse_and_display() {
        assert_eq!(
            OutcomeStatus::parse("success"),
            Some(OutcomeStatus::Success)
        );
        assert_eq!(
            OutcomeStatus::parse("FAILURE"),
            Some(OutcomeStatus::Failure)
        );
        assert_eq!(OutcomeStatus::parse("failed"), Some(OutcomeStatus::Failure));
        assert_eq!(
            OutcomeStatus::parse("partial"),
            Some(OutcomeStatus::Partial)
        );
        assert_eq!(OutcomeStatus::parse("unknown"), None);

        assert_eq!(OutcomeStatus::Success.as_str(), "success");
        assert_eq!(OutcomeStatus::Failure.as_str(), "failure");
        assert_eq!(OutcomeStatus::Partial.as_str(), "partial");
    }

    #[test]
    fn test_episode_record_serde_roundtrip() {
        let rec = EpisodeRecord {
            memory_id: "mem_1".into(),
            task: "deploy the service".into(),
            task_type: Some("deploy".into()),
            status: OutcomeStatus::Failure,
            cause: Some("wrong flag".into()),
            lesson: Some("check flags before running".into()),
            lesson_memory_id: Some("mem_lesson".into()),
            occurred_at: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&rec).unwrap();
        let back: EpisodeRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.memory_id, rec.memory_id);
        assert_eq!(back.status, OutcomeStatus::Failure);
        assert_eq!(back.task_type.as_deref(), Some("deploy"));
        assert_eq!(back.lesson_memory_id.as_deref(), Some("mem_lesson"));
    }

    #[test]
    fn test_episode_filter_defaults() {
        let f = EpisodeFilter::default();
        assert!(f.task_type.is_none());
        assert!(f.status.is_none());
        assert!(f.namespace.is_none());
        assert_eq!(f.limit, 50);
    }

    #[test]
    fn test_memory_without_superseded_by_compat() {
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };
        let mem = Memory::new(
            MemoryType::Fact,
            "plain fact".to_string(),
            Priority::Reference,
            agent,
        );
        let json = serde_json::to_string(&mem).unwrap();
        assert!(
            !json.contains("superseded_by"),
            "unset superseded_by must not appear in serialized output"
        );
        let deserialized: Memory = serde_json::from_str(&json).unwrap();
        assert!(deserialized.superseded_by.is_none());
    }

    #[test]
    fn test_hash_api_keys_in_place_hashes_present_keys() {
        let mut config = AgentRegistryConfig {
            agents: vec![
                AgentProfile {
                    id: "with-key".into(),
                    agent_type: "general".into(),
                    description: String::new(),
                    inject_rules: InjectRules::default(),
                    api_key: Some("the-secret".into()),
                    inject_channel: None,
                },
                AgentProfile {
                    id: "no-key".into(),
                    agent_type: "general".into(),
                    description: String::new(),
                    inject_rules: InjectRules::default(),
                    api_key: None,
                    inject_channel: None,
                },
                AgentProfile {
                    id: "empty-key".into(),
                    agent_type: "general".into(),
                    description: String::new(),
                    inject_rules: InjectRules::default(),
                    api_key: Some("".into()),
                    inject_channel: None,
                },
            ],
        };

        config.hash_api_keys_in_place();

        let with_key = config.agents.iter().find(|a| a.id == "with-key").unwrap();
        let hashed = with_key
            .api_key
            .as_ref()
            .expect("present key must be retained as a hash");
        assert_ne!(hashed, "the-secret", "plaintext key must never be stored");
        assert!(!hashed.is_empty());

        assert!(
            config
                .agents
                .iter()
                .find(|a| a.id == "no-key")
                .unwrap()
                .api_key
                .is_none(),
            "missing key stays None"
        );
        assert!(
            config
                .agents
                .iter()
                .find(|a| a.id == "empty-key")
                .unwrap()
                .api_key
                .is_none(),
            "empty key must be dropped"
        );
    }
}
