//! Scenario profiles: named bundles of `MEMVAULT_*` defaults.
//!
//! # Why this exists
//!
//! MemVault's behaviour is environment-driven (`.env.example` is ~12KB of
//! switches), which is the right *mechanism* and the wrong *onboarding*. A user
//! running Claude Code and Cursor does not want to reason about fourteen
//! independent booleans to get a sensible setup; they want to say "this is a
//! coding vault" and get one.
//!
//! A profile is exactly that: a named, ordered bundle of defaults with a reason
//! attached to each entry. It is **not** a new configuration mechanism — it
//! resolves to the same environment variables every reader already consults, so
//! anything a profile sets can still be overridden per-key, and `status` reports
//! the same provenance it always did.
//!
//! # Precedence
//!
//! Profiles are the **lowest** layer, matching the "nearest wins" rule:
//!
//! ```text
//!   process environment        (highest — an explicit export is a decision)
//!     └─▶ .env file
//!           └─▶ MEMVAULT_PROFILE defaults   ◀── this module
//!                 └─▶ each reader's built-in default
//! ```
//!
//! A profile therefore only ever fills a key that nothing else supplied. It
//! never clobbers: [`apply_to_env`] skips any key already present in the process
//! environment, and [`ScenarioProfile::render_env`] is consumed by
//! `memvault-cli init`, which merges rather than overwrites. This is the
//! property that makes profiles safe to leave set permanently — upgrading
//! MemVault or hand-tuning one key cannot be silently undone by the profile.
//!
//! # What a profile deliberately does not contain
//!
//! **Secrets.** A profile names a provider (`native`, `ollama`, `openai`) and
//! never an API key. Writing a key into a generated `.env` would put a secret in
//! a file users routinely commit or share, and `SECURITY.md` says config is
//! environment-based with keys never committed.
//!
//! **Dimensions MemVault does not have.** The comparable feature in
//! agent-memory (§13.1) configures eleven axes including graph backend, bi-
//! temporal on/off, multimodal normalisers and edge/cloud/hybrid deployment.
//! MemVault has no graph backend to select (single-table one-hop, §16), no
//! multimodal input, and no cloud tier — so a profile that exposed those knobs
//! would be theatre. The four profiles below cover only switches that exist.

use std::fmt;

/// One `MEMVAULT_*` setting a profile supplies, with the reason it is there.
///
/// The `why` is not documentation decoration: `memvault-cli init` prints it, so
/// a user accepting a profile can see what each line does instead of inheriting
/// fourteen unexplained booleans. It is also what keeps the profiles honest —
/// an entry whose rationale cannot be written down is an entry that should not be
/// in a profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileSetting {
    pub key: &'static str,
    pub value: &'static str,
    pub why: &'static str,
}

/// A named deployment shape.
///
/// The four variants mirror the scenario presets surveyed in
/// `docs/DESIGN.md` §2.3 (agent-memory §13.3), re-expressed against switches
/// MemVault actually has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScenarioProfile {
    /// Code agents (Claude Code / Cursor / Codex / dsh). Markdown as the source
    /// of truth so the vault is `git`-able and other tools can read it; cheap
    /// writes; nothing ever purged silently.
    CodingAgent,
    /// Personal assistant across devices and topics. SQLite, richer semantics
    /// (relation triples on), automatic forgetting with a long retention window,
    /// dashboard served as the review surface.
    Assistant,
    /// Several agents and/or people sharing one store. Trust machinery on:
    /// identity verification, the multi-agent corroboration gate, and shared
    /// memories exempt from automatic purge.
    Team,
    /// Low-resource, offline, privacy-first. No embedding provider at all —
    /// MUST memories still inject, because §17.3 resolves them deterministically
    /// with zero embedding calls.
    EdgeLite,
}

/// Every profile, in the order `init` and `--help` list them.
pub const ALL: &[ScenarioProfile] = &[
    ScenarioProfile::CodingAgent,
    ScenarioProfile::Assistant,
    ScenarioProfile::Team,
    ScenarioProfile::EdgeLite,
];

/// The environment variable that selects a profile at runtime.
pub const PROFILE_ENV_KEY: &str = "MEMVAULT_PROFILE";

impl ScenarioProfile {
    /// Parse a profile name. Accepts the CLI spellings (`coding-agent`,
    /// `edge-lite`) and their underscore/snake variants, case-insensitively.
    ///
    /// Returns `None` for an unknown name rather than falling back to a default:
    /// silently substituting a different profile would apply fourteen settings
    /// the user never asked for, which is worse than failing. Callers are
    /// expected to report the valid names — see `VALID_NAMES`.
    pub fn parse(s: &str) -> Option<Self> {
        let norm = s.trim().to_ascii_lowercase().replace('_', "-");
        match norm.as_str() {
            "coding-agent" | "coding" | "code" => Some(Self::CodingAgent),
            "assistant" | "personal" => Some(Self::Assistant),
            "team" | "shared" => Some(Self::Team),
            "edge-lite" | "edge" | "lite" => Some(Self::EdgeLite),
            _ => None,
        }
    }

    /// Canonical CLI spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CodingAgent => "coding-agent",
            Self::Assistant => "assistant",
            Self::Team => "team",
            Self::EdgeLite => "edge-lite",
        }
    }

    /// One-line description for `init --list` and `--help`.
    pub fn description(&self) -> &'static str {
        match self {
            Self::CodingAgent => {
                "Markdown vault (git-able, readable by other tools), native in-process \
                 embeddings, cheap writes, nothing purged silently"
            }
            Self::Assistant => {
                "SQLite vault, native embeddings, relation triples on, automatic \
                 forgetting with a long retention window, dashboard served"
            }
            Self::Team => {
                "SQLite vault with the trust machinery on: identity verification, the \
                 multi-agent corroboration gate, shared memories exempt from auto-purge"
            }
            Self::EdgeLite => {
                "SQLite vault, no embedding provider, minimal pool — MUST memories still \
                 inject because they resolve deterministically (§17.3)"
            }
        }
    }

    /// The settings this profile supplies, in the order they should be written.
    ///
    /// Ordered by concern (storage → retrieval → write pipeline → trust →
    /// forgetting → serving) so the generated `.env` reads top-to-bottom rather
    /// than as an arbitrary dump.
    pub fn settings(&self) -> Vec<ProfileSetting> {
        let s = |key: &'static str, value: &'static str, why: &'static str| ProfileSetting {
            key,
            value,
            why,
        };
        match self {
            Self::CodingAgent => vec![
                s(
                    "MEMVAULT_STORAGE_BACKEND",
                    "markdown",
                    "Files are the source of truth and the cache is a rebuildable shadow index (§18.2), so the vault is greppable, git-able, and readable by tools that speak nothing but Markdown",
                ),
                s(
                    "MEMVAULT_MARKDOWN_DIR",
                    "~/.memvault/markdown",
                    "Required with the markdown backend — MemVault refuses to start without it rather than guessing a directory",
                ),
                s(
                    "MEMVAULT_MARKDOWN_TRUST_MODE",
                    "owned",
                    "Local single-user vault: a file you can edit is a user action, so the file wins byte-for-byte (§18.3.1). Switch to `quarantine` before sharing this directory with any process you do not trust",
                ),
                s(
                    "MEMVAULT_EMBEDDING_PROVIDER",
                    "native",
                    "In-process native inference: no server, no network, no API key, models cached locally",
                ),
                s(
                    "MEMVAULT_RELATIONS",
                    "false",
                    "Relation triples cost an LLM call per write. A coding corpus accumulates fast, and §16 defers graph work until relation volume outgrows single-table one-hop",
                ),
                s(
                    "MEMVAULT_DELTA_WRITE",
                    "true",
                    "Coding memory accumulates near-duplicates ('use pnpm', 'prefer pnpm over npm'); delta-write absorbs the residual instead of piling up copies (§17.1)",
                ),
                s(
                    "MEMVAULT_INJECT_REQUIRE_REVIEW",
                    "true",
                    "Agent-extracted drafts stay out of injection until a human approves them (§14.8)",
                ),
                s(
                    "MEMVAULT_FORGET_AUTO_ENABLED",
                    "false",
                    "Auto-forget is irreversible — it purges the live row, every history snapshot, and the compliance rows. A code vault holding project decisions should not quietly erase them on a timer",
                ),
            ],
            Self::Assistant => vec![
                s(
                    "MEMVAULT_STORAGE_BACKEND",
                    "sqlite",
                    "Single file, no directory to manage, strongest retrieval path",
                ),
                s(
                    "MEMVAULT_EMBEDDING_PROVIDER",
                    "native",
                    "In-process native inference: semantic search with no server and no API key",
                ),
                s(
                    "MEMVAULT_RELATIONS",
                    "true",
                    "A personal profile is mostly entities and preferences, which is where one-hop relation expansion earns its cost",
                ),
                s(
                    "MEMVAULT_DELTA_WRITE",
                    "true",
                    "Long-running personal memory converges instead of accumulating near-copies (§17.1)",
                ),
                s(
                    "MEMVAULT_INJECT_REQUIRE_REVIEW",
                    "true",
                    "Auto-extracted drafts stay out of injection until reviewed (§14.8)",
                ),
                s(
                    "MEMVAULT_FORGET_AUTO_ENABLED",
                    "true",
                    "A personal assistant runs for years; without a purge the archive grows without bound. Retention below keeps it slow enough to reverse by hand first",
                ),
                s(
                    "MEMVAULT_FORGET_RETENTION_DAYS",
                    "180",
                    "Six months in `archived:*` before permanent erasure — long enough to notice and restore, short enough to actually bound growth",
                ),
                s(
                    "MEMVAULT_FORGET_MUST_EXEMPT",
                    "true",
                    "A MUST is a binding instruction, not a stale fact; it should never be purged by a timer",
                ),
                s(
                    "MEMVAULT_SERVE_WEB",
                    "true",
                    "The dashboard is the review surface — with review required, it has to be reachable",
                ),
            ],
            Self::Team => vec![
                s(
                    "MEMVAULT_STORAGE_BACKEND",
                    "sqlite",
                    "Concurrent multi-client access; the markdown backend's advisory single-writer lock is not built for it",
                ),
                s(
                    "MEMVAULT_EMBEDDING_PROVIDER",
                    "native",
                    "In-process native inference, so a shared deployment needs no external embedding service",
                ),
                s(
                    "MEMVAULT_IDENTITY_VERIFICATION",
                    "true",
                    "Distinguishes a cryptographically vouched-for agent_id from a self-declared one. Without it the corroboration gate below counts assertions, not identities",
                ),
                s(
                    "MEMVAULT_CORROBORATION_GATE",
                    "true",
                    "A REFERENCE memory may only be promoted to a binding MUST when independently corroborated, so one spoofed or compromised agent cannot unilaterally inject an instruction everyone obeys (§14.7)",
                ),
                s(
                    "MEMVAULT_CORROBORATION_MIN_AGENTS",
                    "2",
                    "Two distinct identity-verified agents is the minimum for 'independent'; one is just an assertion",
                ),
                s(
                    "MEMVAULT_INJECT_REQUIRE_REVIEW",
                    "true",
                    "In a shared store, an unreviewed draft reaching every session is a much larger blast radius than on a personal vault (§14.8)",
                ),
                s(
                    "MEMVAULT_FORGET_SHARED_AUTO_EXEMPT",
                    "true",
                    "Another agent may depend on a shared memory; the automatic purge path must not remove it out from under them",
                ),
                s(
                    "MEMVAULT_FORGET_AUTO_ENABLED",
                    "false",
                    "Irreversible erasure across a team is a decision, not a default. Use `memvault-cli forget` with an explicit dry-run first",
                ),
                s(
                    "MEMVAULT_MARKDOWN_TRUST_MODE",
                    "quarantine",
                    "Inert under the sqlite backend, and set anyway on purpose: if anyone switches this vault to markdown, `owned` would hand session-injection authority to everyone who can write the directory (§18.3.1). Fail safe rather than fail later",
                ),
                s(
                    "MEMVAULT_DB_POOL_SIZE",
                    "16",
                    "Multiple concurrent MCP/SSE clients plus the dashboard; the default pool is sized for one",
                ),
                s(
                    "MEMVAULT_SERVE_WEB",
                    "true",
                    "Review queue and compliance dashboard are how a team sees what is being injected",
                ),
            ],
            Self::EdgeLite => vec![
                s(
                    "MEMVAULT_STORAGE_BACKEND",
                    "sqlite",
                    "One file, no directory, no external service",
                ),
                s(
                    "MEMVAULT_EMBEDDING_PROVIDER",
                    "off",
                    "No model resident and no inference at all. MUST memories still inject: §17.3 resolves them deterministically with zero embedding calls, which is the point of the two-phase design",
                ),
                s(
                    "MEMVAULT_RELATIONS",
                    "false",
                    "No LLM available to extract triples",
                ),
                s(
                    "MEMVAULT_DELTA_WRITE",
                    "true",
                    "Cheap and local, and without it a small store fills with near-duplicates faster",
                ),
                s(
                    "MEMVAULT_INJECT_REQUIRE_REVIEW",
                    "true",
                    "Rule-based extraction on-device is the least accurate extractor MemVault has, so its drafts especially should not self-inject (§14.8)",
                ),
                s(
                    "MEMVAULT_FORGET_AUTO_ENABLED",
                    "false",
                    "Irreversible purge should not run unattended on a device the user may not be watching",
                ),
                s(
                    "MEMVAULT_DB_POOL_SIZE",
                    "2",
                    "One agent at a time; a larger pool is resident memory for nothing",
                ),
                s(
                    "MEMVAULT_SERVE_WEB",
                    "false",
                    "No listener on the machine. Use the CLI, or turn this on explicitly when you want the dashboard",
                ),
            ],
        }
    }

    /// Render as `.env` file content, one `KEY=value` line per setting with its
    /// rationale as a preceding comment.
    ///
    /// Comments are included because a generated file the user is expected to
    /// keep and edit should explain itself; a bare list of fourteen assignments
    /// is indistinguishable from noise six months later.
    pub fn render_env(&self) -> String {
        let keys: Vec<&str> = self.settings().iter().map(|s| s.key).collect();
        self.render_env_subset(&keys)
    }

    /// Render only the given subset of this profile's settings, keeping the
    /// header and per-key rationale comments.
    ///
    /// This is what `memvault-cli init` uses: merging into an existing file must
    /// not rewrite keys the user already set, and must not emit a key that the
    /// process environment already holds (a profile is the lowest-precedence
    /// layer, so writing it into a file would *promote* it above the environment
    /// on the next start — the one way a profile could clobber an override).
    ///
    /// Keys are matched in this profile's own order, so the output stays grouped
    /// by concern regardless of the order `keys` arrives in. An entry in `keys`
    /// that this profile does not define is ignored.
    pub fn render_env_subset(&self, keys: &[&str]) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "# MemVault scenario profile: {name}\n\
             # {desc}\n\
             #\n\
             # Generated by `memvault-cli init --profile {name}`. Every line below is a\n\
             # plain environment variable — delete or change any of them individually.\n\
             # A profile is the LOWEST-precedence layer: an export in your shell or a\n\
             # hand-written line in this file always wins over it.\n\
             # No secrets are written here, by design.\n",
            name = self.as_str(),
            desc = self.description(),
        ));
        let mut written = 0usize;
        for setting in self.settings() {
            if !keys.contains(&setting.key) {
                continue;
            }
            out.push_str(&format!(
                "\n# {}\n{}={}\n",
                setting.why, setting.key, setting.value
            ));
            written += 1;
        }
        if written == 0 {
            out.push_str(
                "\n# (nothing to write — every setting this profile defines is already \
                 set by the environment or by this file)\n",
            );
        }
        out
    }
}

impl fmt::Display for ScenarioProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Comma-separated valid names, for error messages. Kept as a function rather
/// than a const so the list cannot drift from [`ALL`].
pub fn valid_names() -> String {
    ALL.iter()
        .map(|p| p.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Outcome of applying a profile to the process environment.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ApplyReport {
    /// The profile that was applied, or `None` when no/invalid `MEMVAULT_PROFILE`.
    pub profile: Option<ScenarioProfile>,
    /// Keys the profile set (they were previously unset).
    pub applied: Vec<&'static str>,
    /// Keys the profile skipped because something higher-precedence set them.
    pub skipped: Vec<&'static str>,
    /// The raw `MEMVAULT_PROFILE` value, when it did not parse.
    pub invalid: Option<String>,
}

impl ApplyReport {
    pub fn is_empty(&self) -> bool {
        self.profile.is_none() && self.invalid.is_none()
    }
}

/// Apply `MEMVAULT_PROFILE` to the process environment, filling only unset keys.
///
/// Must be called from the same single-threaded window as [`crate::env_file::load`]
/// — before tracing init and before any thread or async task exists — because it
/// writes to the process environment.
///
/// An unparseable profile name is reported in [`ApplyReport::invalid`] and
/// applies nothing. It deliberately does **not** fall back to a default profile:
/// a typo would otherwise silently apply fourteen settings the user never chose.
///
/// # Safety
///
/// Same contract as `env_file::load`: the caller guarantees no other thread
/// exists yet.
pub unsafe fn apply_to_env() -> ApplyReport {
    let raw = match std::env::var(PROFILE_ENV_KEY) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => return ApplyReport::default(),
    };
    let Some(profile) = ScenarioProfile::parse(&raw) else {
        return ApplyReport {
            invalid: Some(raw),
            ..ApplyReport::default()
        };
    };

    let mut report = ApplyReport {
        profile: Some(profile),
        ..ApplyReport::default()
    };
    for setting in profile.settings() {
        if std::env::var_os(setting.key).is_some() {
            report.skipped.push(setting.key);
            continue;
        }
        // SAFETY: the caller guarantees the single-threaded startup window.
        unsafe { std::env::set_var(setting.key, setting.value) };
        report.applied.push(setting.key);
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keys that must never appear in a profile, because a profile is written
    /// into a file users routinely commit or share.
    const FORBIDDEN_KEYS: &[&str] = &[
        "OPENAI_API_KEY",
        "MEMVAULT_EMBEDDING_API_KEY",
        "MEMVAULT_LLM_EXTRACTION_API_KEY",
    ];

    #[test]
    fn test_parse_accepts_canonical_and_variant_spellings() {
        for (input, want) in [
            ("coding-agent", ScenarioProfile::CodingAgent),
            ("CODING_AGENT", ScenarioProfile::CodingAgent),
            ("  coding ", ScenarioProfile::CodingAgent),
            ("assistant", ScenarioProfile::Assistant),
            ("personal", ScenarioProfile::Assistant),
            ("team", ScenarioProfile::Team),
            ("shared", ScenarioProfile::Team),
            ("edge-lite", ScenarioProfile::EdgeLite),
            ("edge_lite", ScenarioProfile::EdgeLite),
            ("lite", ScenarioProfile::EdgeLite),
        ] {
            assert_eq!(ScenarioProfile::parse(input), Some(want), "input {input:?}");
        }
    }

    #[test]
    fn test_parse_rejects_unknown_rather_than_defaulting() {
        // A silent fallback would apply fourteen settings nobody chose.
        for bad in [
            "",
            "  ",
            "enterprise",
            "default",
            "coding_agent_x",
            "sqlite",
        ] {
            assert_eq!(ScenarioProfile::parse(bad), None, "input {bad:?}");
        }
    }

    #[test]
    fn test_as_str_roundtrips_through_parse() {
        for p in ALL {
            assert_eq!(ScenarioProfile::parse(p.as_str()), Some(*p));
        }
    }

    #[test]
    fn test_all_profiles_are_nonempty_and_documented() {
        for p in ALL {
            let settings = p.settings();
            assert!(!settings.is_empty(), "{} has no settings", p.as_str());
            assert!(!p.description().is_empty());
            for s in &settings {
                assert!(
                    s.key.starts_with("MEMVAULT_"),
                    "{}: key {} is not a MEMVAULT_ variable",
                    p.as_str(),
                    s.key
                );
                assert!(
                    !s.value.is_empty(),
                    "{}: {} has an empty value",
                    p.as_str(),
                    s.key
                );
                // An entry whose rationale cannot be written down is an entry
                // that should not be in a profile.
                assert!(
                    s.why.len() > 20,
                    "{}: {} has a rationale too short to be useful",
                    p.as_str(),
                    s.key
                );
            }
        }
    }

    #[test]
    fn test_no_profile_writes_a_secret() {
        for p in ALL {
            for s in p.settings() {
                assert!(
                    !FORBIDDEN_KEYS.contains(&s.key),
                    "{} must not write {} into a generated file",
                    p.as_str(),
                    s.key
                );
                assert!(
                    !s.key.ends_with("API_KEY"),
                    "{} must not write any *_API_KEY",
                    p.as_str()
                );
                // A profile names a provider; it never carries a credential.
                assert!(
                    !s.value.contains("sk-"),
                    "{}: {} looks like it carries a key",
                    p.as_str(),
                    s.key
                );
            }
        }
    }

    #[test]
    fn test_no_profile_sets_a_key_twice() {
        for p in ALL {
            let keys: Vec<&str> = p.settings().iter().map(|s| s.key).collect();
            let mut sorted = keys.clone();
            sorted.sort_unstable();
            let before = sorted.len();
            sorted.dedup();
            assert_eq!(
                before,
                sorted.len(),
                "{} sets a duplicate key: {:?}",
                p.as_str(),
                keys
            );
        }
    }

    #[test]
    fn test_markdown_backend_always_ships_its_required_dir() {
        // MemVault refuses to start when the markdown backend has no directory,
        // so a profile that selects markdown without one produces a vault that
        // cannot boot.
        for p in ALL {
            let settings = p.settings();
            let backend = settings
                .iter()
                .find(|s| s.key == "MEMVAULT_STORAGE_BACKEND");
            if backend.is_some_and(|b| b.value == "markdown") {
                assert!(
                    settings.iter().any(|s| s.key == "MEMVAULT_MARKDOWN_DIR"),
                    "{} selects the markdown backend without MEMVAULT_MARKDOWN_DIR",
                    p.as_str()
                );
            }
        }
    }

    #[test]
    fn test_team_profile_enables_the_trust_machinery_together() {
        // The corroboration gate counts *identity-verified* agents. Enabling the
        // gate without identity verification would count self-declared agent_ids,
        // which is exactly the spoofing the gate exists to stop (§14.7).
        let settings = ScenarioProfile::Team.settings();
        let get = |k: &str| settings.iter().find(|s| s.key == k).map(|s| s.value);
        assert_eq!(get("MEMVAULT_CORROBORATION_GATE").unwrap(), "true");
        assert_eq!(get("MEMVAULT_IDENTITY_VERIFICATION").unwrap(), "true");
        assert_eq!(get("MEMVAULT_CORROBORATION_MIN_AGENTS").unwrap(), "2");
        assert_eq!(get("MEMVAULT_FORGET_SHARED_AUTO_EXEMPT").unwrap(), "true");
    }

    #[test]
    fn test_edge_lite_turns_the_expensive_paths_off() {
        let settings = ScenarioProfile::EdgeLite.settings();
        let get = |k: &str| settings.iter().find(|s| s.key == k).map(|s| s.value);
        assert_eq!(get("MEMVAULT_EMBEDDING_PROVIDER").unwrap(), "off");
        assert_eq!(get("MEMVAULT_RELATIONS").unwrap(), "false");
        assert_eq!(get("MEMVAULT_SERVE_WEB").unwrap(), "false");
    }

    #[test]
    fn test_render_env_is_parseable_by_the_env_file_reader() {
        // The generated file must survive a round trip through the same parser
        // that reads a hand-written one, or `init` writes something MemVault
        // cannot load.
        for p in ALL {
            let rendered = p.render_env();
            let (pairs, warnings) = crate::env_file::parse(&rendered);
            assert!(
                warnings.is_empty(),
                "{} produced parse warnings: {:?}",
                p.as_str(),
                warnings
            );
            assert_eq!(
                pairs.len(),
                p.settings().len(),
                "{}: rendered {} pairs from {} settings",
                p.as_str(),
                pairs.len(),
                p.settings().len()
            );
            for (setting, (key, value)) in p.settings().iter().zip(pairs) {
                assert_eq!(setting.key, key);
                assert_eq!(setting.value, value);
            }
            assert!(
                rendered.contains(
                    &format!("MEMVAULT profile: {}", p.as_str())
                        .replace("MEMVAULT profile", "MemVault scenario profile")
                ) || rendered.contains(p.as_str())
            );
        }
    }

    #[test]
    fn test_valid_names_lists_every_profile() {
        let names = valid_names();
        for p in ALL {
            assert!(
                names.contains(p.as_str()),
                "{:?} missing {}",
                names,
                p.as_str()
            );
        }
    }

    #[test]
    fn test_display_matches_as_str() {
        for p in ALL {
            assert_eq!(p.to_string(), p.as_str());
        }
    }

    /// Serializes every test that mutates the process environment, matching the
    /// pattern `env_file` and `embedding` use.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Run `f` with `keys` cleared from the process environment, then restore
    /// them exactly. Serialized by `ENV_LOCK` because Rust runs tests on parallel
    /// threads inside one process, and an unguarded env mutation here would be a
    /// flaky test for every other suite that reads the environment.
    ///
    /// `f` receives a setter so the test body never touches `set_var` directly:
    /// keeping every mutation inside this one function is what makes the unsafe
    /// auditable in one place rather than scattered across eight tests.
    fn with_clean_env<F: FnOnce(&EnvSetter)>(keys: &[&str], f: F) {
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved: Vec<(String, Option<String>)> = keys
            .iter()
            .map(|k| (k.to_string(), std::env::var(k).ok()))
            .collect();
        // SAFETY: every env mutation in this module happens under `ENV_LOCK`, and
        // the tests that call this are `#[test]` (single-threaded within the
        // guard). Mirrors the convention in `env_file`'s own tests.
        unsafe {
            for k in keys {
                std::env::remove_var(k);
            }
        }
        f(&EnvSetter);
        unsafe {
            for (k, v) in saved {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
        drop(guard);
    }

    /// Test-only env setter, so the unsafe blocks stay in `with_clean_env`.
    struct EnvSetter;

    impl EnvSetter {
        fn set(&self, key: &str, value: &str) {
            // SAFETY: called only inside `with_clean_env`, under `ENV_LOCK`.
            unsafe { std::env::set_var(key, value) };
        }
    }

    #[test]
    fn test_apply_to_env_fills_unset_keys_and_reports_them() {
        let touched: Vec<&str> = ScenarioProfile::EdgeLite
            .settings()
            .iter()
            .map(|s| s.key)
            .chain([PROFILE_ENV_KEY])
            .collect();
        with_clean_env(&touched, |env| {
            env.set(PROFILE_ENV_KEY, "edge-lite");
            // SAFETY: single-threaded inside the guard.
            let report = unsafe { apply_to_env() };
            assert_eq!(report.profile, Some(ScenarioProfile::EdgeLite));
            assert!(!report.applied.is_empty());
            assert!(report.skipped.is_empty());
            assert_eq!(std::env::var("MEMVAULT_EMBEDDING_PROVIDER").unwrap(), "off");
        });
    }

    #[test]
    fn test_apply_to_env_never_clobbers_a_higher_precedence_value() {
        let touched: Vec<&str> = ScenarioProfile::EdgeLite
            .settings()
            .iter()
            .map(|s| s.key)
            .chain([PROFILE_ENV_KEY])
            .collect();
        with_clean_env(&touched, |env| {
            env.set(PROFILE_ENV_KEY, "edge-lite");
            // An explicit export is a decision; the profile must not undo it.
            env.set("MEMVAULT_EMBEDDING_PROVIDER", "native");
            // SAFETY: single-threaded inside the guard.
            let report = unsafe { apply_to_env() };
            assert!(report.skipped.contains(&"MEMVAULT_EMBEDDING_PROVIDER"));
            assert_eq!(
                std::env::var("MEMVAULT_EMBEDDING_PROVIDER").unwrap(),
                "native",
                "a profile must never overwrite an explicitly set value"
            );
        });
    }

    #[test]
    fn test_apply_to_env_reports_an_invalid_name_and_applies_nothing() {
        let touched: Vec<&str> = ScenarioProfile::EdgeLite
            .settings()
            .iter()
            .map(|s| s.key)
            .chain([PROFILE_ENV_KEY])
            .collect();
        with_clean_env(&touched, |env| {
            env.set(PROFILE_ENV_KEY, "enterprize");
            // SAFETY: single-threaded inside the guard.
            let report = unsafe { apply_to_env() };
            assert_eq!(report.invalid.as_deref(), Some("enterprize"));
            assert!(report.applied.is_empty());
            assert!(
                std::env::var("MEMVAULT_EMBEDDING_PROVIDER").is_err(),
                "an unparseable profile must not apply a fallback"
            );
        });
    }

    #[test]
    fn test_apply_to_env_is_a_noop_without_the_variable() {
        with_clean_env(&[PROFILE_ENV_KEY], |_env| {
            // SAFETY: single-threaded inside the guard.
            let report = unsafe { apply_to_env() };
            assert!(report.is_empty());
            assert_eq!(report.profile, None);
            assert!(report.applied.is_empty());
        });
    }

    #[test]
    fn test_apply_to_env_ignores_an_empty_value() {
        with_clean_env(&[PROFILE_ENV_KEY], |env| {
            env.set(PROFILE_ENV_KEY, "   ");
            // SAFETY: single-threaded inside the guard.
            let report = unsafe { apply_to_env() };
            assert!(
                report.is_empty(),
                "whitespace-only is 'unset', not 'invalid'"
            );
        });
    }
}
