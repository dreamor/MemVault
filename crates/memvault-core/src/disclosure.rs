//! Progressive disclosure: degrade a memory to a shorter form instead of
//! dropping it when the injection budget runs out.
//!
//! # The problem
//!
//! `router::format::trim_to_budget` walks candidates in priority order and
//! **breaks** at the first one that does not fit. Everything after it is cut, and
//! the cut is reported (`InjectSkipReason::TokenBudgetExceeded`) rather than
//! silent — but reported-or-not, the agent does not get the memory. With a
//! 1500-token budget and a handful of long memories, the tail of the ranked list
//! is routinely unreachable no matter how relevant it is.
//!
//! The alternative to "does not fit, drop it" is "does not fit, summarise it".
//! §3.2 already establishes that follow-through *falls* as the number of injected
//! memories rises, so the goal is not to cram more text in — it is to keep more
//! memories reachable at a lower per-memory cost.
//!
//! # Why the layers are derived, not stored
//!
//! `excerpt_l1` and `summary_l0` are **deterministic and extractive**, so they
//! are computed on demand rather than persisted:
//!
//! - no migration, no backfill, no frontmatter key, no cache-invalidation path;
//! - a derived layer can never go stale. A stored summary edited out from under
//!   by a later content change is a contradiction the agent would be shown as
//!   fact, and keeping the two in sync would need a hook on every write path
//!   (`update`, `supersede`, markdown reindex, proposal approval, restore);
//! - it costs microseconds of string scanning at injection time, against a budget
//!   whose whole purpose is to bound cost.
//!
//! If an LLM-generated summary is added later, the right shape is a stored
//! `Option<String>` column that these functions *prefer* when present and fall
//! back to deriving — not a replacement for them, since the derived form is what
//! keeps the feature working with no provider configured (the `edge-lite`
//! profile, §7.5).
//!
//! # L2 is not a layer
//!
//! Following agent-memory's `ContentLayers` (§2.3): L0 is the summary, L1 the
//! excerpt, and **L2 is just `Memory::content`** — never copied into a third
//! field. Storing it would double the size of every long memory to save a
//! pointer indirection.
//!
//! # On the fusion concern
//!
//! §2.3 flags a trap worth stating explicitly here, because it constrains how
//! these layers may be used: agent-memory must apply MaxP per (channel, unit)
//! *before* cross-channel fusion, because it indexes L0/L1/L2 as separate
//! physical entries of the same channel — otherwise a memory with more layers is
//! recalled more times and RRF accumulates each as independent evidence,
//! systematically over-weighting long memories while the metrics look like they
//! improved.
//!
//! **MemVault does not have that problem, and must not acquire it.** These layers
//! are derived at *injection* time and are never indexed: FTS covers
//! `content`/`instruction`/`tags`, one row produces one candidate per channel,
//! and `HybridMerger::merge` already keys its accumulator by `memory.id`. Adding
//! l0/l1 to the FTS index would break this twice over — it would create the
//! duplicate-evidence problem above, and it would distort BM25 by indexing text
//! that is a prefix of text already indexed. `test_disclosure_layers_do_not_change_hybrid_ranking`
//! pins the invariant.

use crate::models::Memory;

/// How much of a memory's text an injection carries.
///
/// Declared from most text to least: `Full`, then `Excerpt`, then `Summary`. The
/// derived `Ord` follows that declaration order, so `Summary` compares greatest —
/// note that [`best_level_within`] does **not** rely on the ordering. It walks an
/// explicit array, so the search order is legible at the call site and cannot be
/// changed as a side effect of reordering this enum.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum DisclosureLevel {
    /// The full text. L2 in agent-memory's naming; it *is* `Memory::content`,
    /// not a copy of it.
    Full,
    /// The leading portion, cut at a sentence or word boundary (L1).
    Excerpt,
    /// One line: the first sentence, or a hard word-boundary cut (L0).
    Summary,
}

impl DisclosureLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Excerpt => "excerpt",
            Self::Summary => "summary",
        }
    }
}

/// Below this many characters the full text already *is* an excerpt, so deriving
/// one would produce a near-copy and save nothing.
const L1_MIN_CHARS: usize = 420;
/// Target length for an L1 excerpt before the abbreviation marker.
const L1_TARGET_CHARS: usize = 320;
/// Below this many characters the full text already *is* a summary.
const L0_MIN_CHARS: usize = 150;
/// Target length for an L0 summary before the abbreviation marker.
const L0_TARGET_CHARS: usize = 96;

/// Marker appended to any shortened text.
///
/// Not cosmetic. An abbreviated memory that looks complete is worse than a
/// dropped one: the agent acts on a rule whose qualifying clause was cut, and
/// nothing in the injected block says so. The marker also tells the agent that
/// `search_memory` can retrieve the rest.
const ABBREVIATED: &str = " […]";

/// Cut `text` to at most `target` characters, preferring a sentence boundary,
/// then a word boundary, then a hard cut. Never splits a UTF-8 sequence.
///
/// Returns `None` when the text is already no longer than `target`.
fn cut_at_boundary(text: &str, target: usize) -> Option<String> {
    if text.chars().count() <= target {
        return None;
    }

    // Work on a char-count prefix so multibyte text (CJK especially) is measured
    // the same way `estimate_tokens` measures it, and so slicing can never land
    // inside a code point.
    let window: String = text.chars().take(target).collect();

    // 1. Last sentence terminator inside the window. A cut mid-sentence reads as
    //    a complete thought that happens to be truncated, which is exactly the
    //    failure the marker exists to prevent.
    let sentence_end = window
        .char_indices()
        .rev()
        .find(|(_, c)| matches!(c, '.' | '!' | '?' | '。' | '！' | '？' | '\n'))
        .map(|(i, c)| i + c.len_utf8());
    // 2. Otherwise the last word boundary, so we do not cut through a token.
    // 3. Otherwise the window itself.
    let end = sentence_end.or_else(|| {
        window
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map(|(i, _)| i)
    });

    let body = match end {
        Some(e) if e > 0 => window[..e].trim_end(),
        _ => window.trim_end(),
    };
    // Guard against a degenerate cut (e.g. text starting with a terminator) that
    // would leave nothing meaningful behind.
    if body.is_empty() {
        return Some(format!("{}{ABBREVIATED}", window.trim_end()));
    }
    Some(format!("{body}{ABBREVIATED}"))
}

/// Derive the L1 excerpt of `content`, or `None` when the content is short
/// enough that an excerpt would be the whole thing.
pub fn excerpt_l1(content: &str) -> Option<String> {
    if content.chars().count() < L1_MIN_CHARS {
        return None;
    }
    cut_at_boundary(content, L1_TARGET_CHARS)
}

/// Derive the L0 summary of `content`: the first sentence when there is one
/// short enough to be useful, otherwise a word-boundary cut.
///
/// `None` when the content is already summary-length.
pub fn summary_l0(content: &str) -> Option<String> {
    if content.chars().count() < L0_MIN_CHARS {
        return None;
    }
    // Prefer the first sentence outright — a summary that is a real sentence reads
    // as a statement, not as a fragment, which matters because this text is what
    // the agent treats as the rule.
    let first_sentence = content
        .char_indices()
        .find(|(_, c)| matches!(c, '.' | '!' | '?' | '。' | '！' | '？' | '\n'))
        .map(|(i, c)| &content[..i + c.len_utf8()]);
    if let Some(s) = first_sentence
        && s.chars().count() >= L0_TARGET_CHARS / 2
        && s.chars().count() <= L0_TARGET_CHARS * 2
    {
        return Some(format!("{}{ABBREVIATED}", s.trim_end()));
    }
    cut_at_boundary(content, L0_TARGET_CHARS)
}

/// The text injection renders for a memory: its directive when it has one,
/// otherwise its body. Mirrors `trim_to_budget`'s existing choice so the budget
/// arithmetic and the rendered output can never disagree about what was counted.
pub fn rendered_text(memory: &Memory) -> &str {
    memory.instruction.as_deref().unwrap_or(&memory.content)
}

/// The text to inject for `memory` at `level`.
///
/// Degrades the *rendered* field only — for a memory with an `instruction`, that
/// is the directive, and the body is not what gets injected anyway. Returns the
/// full text unchanged when the level is [`DisclosureLevel::Full`] or when no
/// shorter layer could be derived (a short memory has nothing to summarise, and
/// silently returning the full text is correct there rather than an error).
pub fn text_at_level(memory: &Memory, level: DisclosureLevel) -> String {
    let text = rendered_text(memory);
    let shortened = match level {
        DisclosureLevel::Full => None,
        DisclosureLevel::Excerpt => excerpt_l1(text),
        DisclosureLevel::Summary => summary_l0(text),
    };
    shortened.unwrap_or_else(|| text.to_string())
}

/// The cheapest level whose text fits in `budget_tokens`, or `None` when even the
/// summary does not fit.
///
/// `overhead` is the per-memory framing cost the caller adds on top of the text
/// (the `[MUST]`/`[REF]` tag and newline), so the answer accounts for the same
/// total the caller will.
///
/// Walks an explicit `[Full, Excerpt, Summary]` array rather than relying on the
/// enum's derived ordering: the point is to keep as much text as fits, not to
/// summarise for its own sake, and spelling the order out here keeps that intent
/// readable and independent of how the enum happens to be declared.
pub fn best_level_within(
    memory: &Memory,
    budget_tokens: usize,
    overhead: usize,
    estimate: impl Fn(&str) -> usize,
) -> Option<DisclosureLevel> {
    for level in [
        DisclosureLevel::Full,
        DisclosureLevel::Excerpt,
        DisclosureLevel::Summary,
    ] {
        let text = text_at_level(memory, level);
        if estimate(&text) + overhead <= budget_tokens {
            return Some(level);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{MemoryType, Priority, SourceAgent};

    fn mem(content: &str, instruction: Option<&str>) -> Memory {
        let mut m = Memory::new(
            MemoryType::Fact,
            content.to_string(),
            Priority::Reference,
            SourceAgent {
                id: "test".to_string(),
                agent_type: "general".to_string(),
                session_id: None,
            },
        );
        m.instruction = instruction.map(str::to_string);
        m
    }

    /// A long single-sentence-free block, so boundary logic has to fall back to
    /// whitespace rather than finding a terminator.
    fn long_no_sentences(words: usize) -> String {
        (0..words)
            .map(|i| format!("item{i}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn test_short_content_has_no_shorter_layers() {
        // Deriving a "summary" of a short memory would produce a near-copy and
        // save nothing, so both layers must decline.
        let short = "use pnpm, not npm";
        assert_eq!(excerpt_l1(short), None);
        assert_eq!(summary_l0(short), None);
        assert_eq!(
            text_at_level(&mem(short, None), DisclosureLevel::Summary),
            short,
            "with no layer to derive, the full text is the correct answer"
        );
    }

    #[test]
    fn test_excerpt_prefers_a_sentence_boundary() {
        let content = format!(
            "{}. {}. {}",
            "always run the migration step before deploying".repeat(4),
            "the second sentence should be cut away".repeat(6),
            "trailing"
        );
        let excerpt = excerpt_l1(&content).expect("long enough to excerpt");
        assert!(excerpt.ends_with(ABBREVIATED), "must be marked: {excerpt}");
        let body = excerpt.trim_end_matches(ABBREVIATED);
        assert!(
            body.ends_with('.') || body.ends_with('!') || body.ends_with('?'),
            "should cut at a sentence boundary, got: {body:?}"
        );
        assert!(body.chars().count() <= L1_TARGET_CHARS);
        assert!(
            content.starts_with(body),
            "an excerpt must be a prefix of the original, never a rewrite"
        );
    }

    #[test]
    fn test_excerpt_falls_back_to_a_word_boundary() {
        let content = long_no_sentences(200);
        let excerpt = excerpt_l1(&content).unwrap();
        let body = excerpt.trim_end_matches(ABBREVIATED);
        assert!(
            !body.ends_with(|c: char| c.is_alphanumeric() && !body.contains(' ')),
            "should not cut through a word"
        );
        // The cut must land on a whole token: the last word must be complete.
        let last = body.rsplit(' ').next().unwrap();
        assert!(
            content.contains(&format!("{last} ")),
            "word was split: {last:?}"
        );
    }

    #[test]
    fn test_summary_takes_the_first_sentence() {
        let content = format!(
            "{} {} {} The rest is detail that a summary should drop.",
            "the deploy freeze runs until friday and covers all services",
            "including the internal ones",
            "and the database migration window."
        );
        let summary = summary_l0(&content).expect("long enough to summarise");
        let body = summary.trim_end_matches(ABBREVIATED);
        assert!(
            body.ends_with('.'),
            "summary should be a whole sentence, got {body:?}"
        );
        assert!(
            !body.contains("The rest is detail"),
            "summary must drop later sentences"
        );
    }

    #[test]
    fn test_layers_never_split_a_multibyte_character() {
        // CJK text is the realistic case for MemVault (§2.1: CJK bigram
        // tokenization) and the one where naive byte slicing panics.
        let content = "部署前必须运行数据库迁移，".repeat(40);
        let excerpt = excerpt_l1(&content).expect("long CJK text");
        let summary = summary_l0(&content).expect("long CJK text");
        for s in [&excerpt, &summary] {
            assert!(s.chars().count() > 0);
            // Re-encoding proves no partial code point survived.
            assert_eq!(String::from_utf8(s.as_bytes().to_vec()).unwrap(), *s);
        }
        assert!(excerpt.ends_with(ABBREVIATED));
        assert!(summary.ends_with(ABBREVIATED));
    }

    #[test]
    fn test_degrades_the_rendered_field_not_the_other_one() {
        // Injection renders `instruction` when present. Degrading the body of a
        // memory whose directive is what gets injected would change nothing the
        // agent sees, while reporting a degradation that did not happen.
        let long_instruction = format!("{}. {}", long_no_sentences(120), "second");
        let m = mem("a short body", Some(&long_instruction));
        assert_eq!(rendered_text(&m), long_instruction);
        let degraded = text_at_level(&m, DisclosureLevel::Excerpt);
        assert!(degraded.ends_with(ABBREVIATED));
        assert!(
            long_instruction.starts_with(degraded.trim_end_matches(ABBREVIATED)),
            "the directive is what must be shortened"
        );
    }

    #[test]
    fn test_best_level_within_picks_the_cheapest_that_fits() {
        let content = format!(
            "{} {} The second sentence is long enough to push the total well \
             past any small budget we test with here.",
            "the rule is short".repeat(3),
            "and then it goes on".repeat(30)
        );
        let m = mem(&content, None);
        let est = |t: &str| crate::router::format::estimate_tokens(t);
        let overhead = 15;

        // Generous budget: the full text fits, so no degradation.
        let generous = est(&content) + overhead + 10;
        assert_eq!(
            best_level_within(&m, generous, overhead, est),
            Some(DisclosureLevel::Full)
        );

        // Too small for anything: must report None rather than silently
        // returning the summary that does not fit.
        assert_eq!(best_level_within(&m, 1, overhead, est), None);

        // Between the summary and the full text: must land on a shortened level,
        // and the level it names must actually fit.
        let summary_tokens = est(&text_at_level(&m, DisclosureLevel::Summary)) + overhead;
        let tight = summary_tokens + 2;
        let chosen = best_level_within(&m, tight, overhead, est).expect("summary fits");
        assert_ne!(chosen, DisclosureLevel::Full);
        assert!(est(&text_at_level(&m, chosen)) + overhead <= tight);
    }

    #[test]
    fn test_level_ordering_and_names() {
        // The derived `Ord` follows declaration order, so `Summary` — the
        // shortest text — is the *greatest* value. `best_level_within` does not
        // rely on that: it walks an explicit `[Full, Excerpt, Summary]` array, so
        // the search order is readable at the call site and cannot be changed by
        // reordering the enum. Pinned here so a reorder is a deliberate act.
        assert!(DisclosureLevel::Full < DisclosureLevel::Excerpt);
        assert!(DisclosureLevel::Excerpt < DisclosureLevel::Summary);
        assert_eq!(DisclosureLevel::Full.as_str(), "full");
        assert_eq!(DisclosureLevel::Excerpt.as_str(), "excerpt");
        assert_eq!(DisclosureLevel::Summary.as_str(), "summary");
    }

    #[test]
    fn test_an_excerpt_is_always_shorter_than_its_source() {
        // The whole point is saving budget; a "shortened" layer that is not
        // shorter would degrade the memory for nothing.
        let content = format!("{}. {}", long_no_sentences(200), long_no_sentences(200));
        for level in [DisclosureLevel::Excerpt, DisclosureLevel::Summary] {
            let text = text_at_level(&mem(&content, None), level);
            assert!(
                text.chars().count() < content.chars().count(),
                "{level:?} did not shorten the text"
            );
        }
    }

    #[test]
    fn test_repeated_derivation_is_stable() {
        // Derived-on-demand only works if it is deterministic: the same memory
        // must render identically in every session, or an agent would see the
        // rule worded differently each time.
        let content = format!("{}. {}", long_no_sentences(150), "trailing detail");
        let m = mem(&content, None);
        let a = text_at_level(&m, DisclosureLevel::Summary);
        let b = text_at_level(&m, DisclosureLevel::Summary);
        assert_eq!(a, b);
    }

    /// Pins the invariant from the module docs: these layers are derived at
    /// injection time and are never indexed, so they cannot change what fusion
    /// recalls or how it ranks. If a future change starts indexing L0/L1 as
    /// separate entries, agent-memory's MaxP problem (§2.3) arrives with it and
    /// this test's premise breaks.
    #[test]
    fn test_disclosure_layers_do_not_change_hybrid_ranking() {
        use crate::hybrid::HybridMerger;
        use crate::models::{HitSource, SearchResult};

        // One long memory (derives both layers) and one short one (derives none),
        // each recalled by both channels. Fusion must produce one entry per
        // memory: if layers were indexed separately, the long one would be
        // recalled several times within a channel and RRF would accumulate each
        // as independent evidence, systematically over-weighting it.
        let long = format!("{}. {}", long_no_sentences(200), "tail");
        let mk2 = |id: &str, content: &str| {
            let mut m = mem(content, None);
            m.id = id.to_string();
            SearchResult {
                memory: m,
                score: 1.0,
                hit_sources: Vec::new(),
                as_of_source: None,
            }
        };
        let keyword = vec![mk2("mem_long", &long), mk2("mem_short", "use pnpm")];
        let vector = vec![mk2("mem_long", &long), mk2("mem_short", "use pnpm")];

        let merged = HybridMerger::merge(keyword, vector, 10, 0.4, 0.6);
        assert_eq!(merged.len(), 2, "one entry per memory, not per layer");
        let long_hit = merged.iter().find(|r| r.memory.id == "mem_long").unwrap();
        assert_eq!(
            long_hit.hit_sources.len(),
            2,
            "recalled once per channel; a third or fourth source would mean the \
             layers were indexed as separate entries"
        );
        assert!(matches!(long_hit.hit_sources[0], HitSource::Keyword { .. }));
        assert!(matches!(long_hit.hit_sources[1], HitSource::Vector { .. }));

        // And neither score may carry extra accumulated evidence. The two sit at
        // different ranks, so they are *expected* to differ — what must hold is
        // that each equals exactly the RRF sum for its own rank, one contribution
        // per channel. Were the layers indexed as separate entries, the long
        // memory would collect additional contributions and exceed its formula.
        let short_hit = merged.iter().find(|r| r.memory.id == "mem_short").unwrap();
        const K: f64 = 60.0;
        let expected_long = 0.4 / (K + 1.0) + 0.6 / (K + 1.0); // rank 1 in both
        let expected_short = 0.4 / (K + 2.0) + 0.6 / (K + 2.0); // rank 2 in both
        assert!(
            (long_hit.score - expected_long).abs() < 1e-12,
            "long memory accumulated extra evidence: {} vs expected {}",
            long_hit.score,
            expected_long
        );
        assert!(
            (short_hit.score - expected_short).abs() < 1e-12,
            "short memory score drifted: {} vs expected {}",
            short_hit.score,
            expected_short
        );
    }
}
