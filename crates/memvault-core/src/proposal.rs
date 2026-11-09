//! Agent-proposed edits to an existing memory, gated through human review.
//!
//! # The problem this solves
//!
//! An agent that can read its own memory should be able to propose corrections —
//! that is Letta's self-editing memory, and it is genuinely useful. Letta's own
//! listed weakness is that it relies entirely on the LLM's judgement about what
//! to write, so it can misrecord. MemVault's answer is not to remove the
//! capability but to put a human between the proposal and the effect.
//!
//! The reason that gap is load-bearing rather than cautious is §18.3.1: write
//! access to the vault **is** session-injection authority. An edit that took
//! effect immediately would let one compromised or spoofed agent rewrite a
//! binding MUST that every other agent then obeys. §14.7's corroboration gate
//! exists to stop a single agent from *creating* that authority; this module
//! exists to stop one from *amending* it.
//!
//! # The design
//!
//! A proposal is a **separate memory**, not an in-place edit:
//!
//! ```text
//!   agent proposes  ─▶  new memory saved with
//!                         human_reviewed = false      ← §14.8's gate keeps it
//!                         ai_generated   = true          out of every injection
//!                         proposes_supersede_of = <target>
//!                       target memory: UNTOUCHED, still injecting
//!
//!   human approves  ─▶  proposal becomes reviewed
//!                       target is superseded by it
//!                       the link is consumed
//!
//!   human rejects   ─▶  proposal is deleted; target never changed
//! ```
//!
//! The critical property is what happens *while the proposal is pending*. The
//! obvious alternative — apply the edit in place and revoke its trust — looks
//! equivalent because an untrusted memory is excluded from injection anyway. It
//! is not equivalent: it means the target stops injecting too, so **any agent
//! could suppress any MUST simply by proposing an edit to it**. That is a
//! denial-of-injection vector, and it is precisely the threat model §14.7 is
//! about. Keeping the target live until a human decides costs one extra row and
//! closes it.
//!
//! While pending, the proposal is also excluded from retrieval
//! (`proposes_supersede_of IS NULL`, alongside the existing `superseded_by`
//! filter): a pending change is not current truth, and returning both a memory
//! and its near-identical pending edit would leave the caller unable to tell
//! which one is real. It stays visible through `list_pending` and the Inbox,
//! which is where it is meant to be read.
//!
//! # On the field name
//!
//! `proposes_supersede_of` is a real column, not a tag. Approving a proposal
//! triggers a destructive action on a *different* row, and an action like that
//! should be keyed on a validated field rather than a string prefix inside a
//! user-editable list.

use crate::error::{MemVaultError, Result};
use crate::models::{Memory, SourceAgent};
use crate::storage::{ApproveSupersedeOutcome, MemoryStore};

/// Which field of a memory an edit targets.
///
/// Explicit rather than inferred. Searching `content` and then falling back to
/// `instruction` would be convenient and wrong: the two are rendered differently
/// at injection time (§6.2 — `instruction` is the directive, `content` is the
/// body), so silently choosing between them would let an agent edit the body
/// while believing it had changed the directive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditField {
    Content,
    Instruction,
}

impl EditField {
    /// Parse the wire spelling. Unknown values are an error, never a default:
    /// defaulting to `Content` would apply an edit to the wrong field.
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "content" | "body" => Ok(Self::Content),
            "instruction" | "directive" => Ok(Self::Instruction),
            other => Err(MemVaultError::InvalidInput(format!(
                "field must be `content` or `instruction`, got {other:?}"
            ))),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Content => "content",
            Self::Instruction => "instruction",
        }
    }
}

/// A successfully created proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    /// The new pending memory's id. This is what the reviewer approves.
    pub proposal_id: String,
    /// The memory it would replace on approval.
    pub target_id: String,
    pub field: EditField,
    /// The target's text before the edit, for the review UI to diff against.
    pub before: String,
    /// The proposed text after the edit.
    pub after: String,
    /// **The edit will not change behaviour.** True when the edited field was
    /// `content` but the target's `instruction` still contains `old_text`.
    ///
    /// This matters more than it looks. Injection renders `instruction` as the
    /// directive and `content` as the body (§6.2), so an agent that "corrects" a
    /// memory by editing only the body leaves the thing every future session
    /// actually obeys saying the old thing — and after approval the memory's body
    /// and directive would actively contradict each other. Detected and reported
    /// rather than silently patched, because rewriting the instruction too would
    /// be a guess about text the caller never asked to change; the right fix is
    /// a second proposal with `field=instruction`, and the caller should decide
    /// that.
    pub directive_still_says_old: bool,
}

/// What [`approve`] did.
#[derive(Debug, Clone)]
pub struct ApprovalOutcome {
    /// The memory that is now reviewed and injectable, as persisted.
    ///
    /// Carried rather than re-fetched because two callers need it: MCP
    /// `review_memory(action=edit)` re-embeds the approved text, and every caller
    /// wants to report what was approved without a second round trip.
    pub approved: Memory,
    /// The target it superseded, when this approval consumed a proposal link.
    /// `None` for an ordinary approval of a memory that was never a proposal.
    pub superseded_target: Option<String>,
}

impl ApprovalOutcome {
    /// Convenience for callers that only need the id.
    pub fn approved_id(&self) -> &str {
        &self.approved.id
    }
}

/// Count non-overlapping occurrences of `needle` in `haystack`.
///
/// Non-overlapping matters: for `old_text = "aa"` in `"aaa"` the answer is 1,
/// not 2. An edit anchored on an overlapping match would replace text the caller
/// did not intend.
fn count_occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack.match_indices(needle).count()
}

/// Create a proposal to replace exactly one occurrence of `old_text` with
/// `new_text` in `target_id`'s `field`.
///
/// **Does not modify the target.** See the module docs for why that is the whole
/// point rather than an implementation detail.
///
/// # Errors
///
/// - target does not exist → [`MemVaultError::NotFound`]
/// - `old_text` empty, equal to `new_text`, or not found → `InvalidInput`
/// - `old_text` occurs more than once → `InvalidInput` naming the count. Never
///   guesses which occurrence was meant and never replaces all of them: an
///   ambiguous anchor is a caller bug, and choosing silently would apply an edit
///   the agent did not ask for.
/// - target is itself superseded, or is itself a pending proposal → `InvalidInput`.
///   Editing archived knowledge should go through `restore` first; chaining
///   proposals would make "which version is pending" unanswerable.
pub async fn propose_text_edit<S: MemoryStore + ?Sized>(
    store: &S,
    target_id: &str,
    old_text: &str,
    new_text: &str,
    field: EditField,
    proposer: SourceAgent,
) -> Result<Proposal> {
    if old_text.is_empty() {
        return Err(MemVaultError::InvalidInput(
            "old_text must not be empty — it anchors the edit".to_string(),
        ));
    }
    if old_text == new_text {
        return Err(MemVaultError::InvalidInput(
            "old_text and new_text are identical; nothing to propose".to_string(),
        ));
    }

    let target = store.get(target_id).await?;
    if target.superseded_by.is_some() {
        return Err(MemVaultError::InvalidInput(format!(
            "{target_id} is superseded by {:?} and is archived knowledge; restore it \
             before proposing an edit",
            target.superseded_by
        )));
    }
    if target.proposes_supersede_of.is_some() {
        return Err(MemVaultError::InvalidInput(format!(
            "{target_id} is itself a pending proposal; approve or reject it first \
             rather than chaining a proposal onto a proposal"
        )));
    }

    let before = match field {
        EditField::Content => target.content.clone(),
        EditField::Instruction => target.instruction.clone().ok_or_else(|| {
            MemVaultError::InvalidInput(format!(
                "{target_id} has no instruction to edit (its `instruction` field is empty)"
            ))
        })?,
    };

    let occurrences = count_occurrences(&before, old_text);
    match occurrences {
        0 => {
            return Err(MemVaultError::InvalidInput(format!(
                "old_text not found in {target_id}'s {} field; nothing was changed. \
                 The match is exact and case-sensitive — re-read the memory before \
                 retrying rather than guessing at a near-match",
                field.as_str()
            )));
        }
        1 => {}
        n => {
            return Err(MemVaultError::InvalidInput(format!(
                "old_text occurs {n} times in {target_id}'s {} field; an edit must be \
                 unambiguous. Widen old_text until it matches exactly one place — \
                 picking one silently could apply an edit nobody asked for",
                field.as_str()
            )));
        }
    }
    let after = before.replacen(old_text, new_text, 1);

    // Computed from the *target*, before the proposal is built: does the
    // directive injection actually renders still contain the text being replaced?
    let directive_still_says_old = field == EditField::Content
        && target
            .instruction
            .as_deref()
            .is_some_and(|inst| inst.contains(old_text));

    // The proposal inherits everything that makes the memory *what it is* — so
    // that approving it is a clean replacement rather than a downgrade — and
    // resets everything that says *whether it may be believed*.
    let mut proposal = Memory::new(
        target.memory_type,
        target.content.clone(),
        target.priority,
        proposer,
    );
    proposal.namespace = target.namespace.clone();
    proposal.tags = target.tags.clone();
    proposal.visibility = target.visibility;
    proposal.layer = target.layer;
    proposal.skill_meta = target.skill_meta.clone();
    // Valid-time carries over: a proposal to reword a temporary fact must not
    // quietly make it permanent, and vice versa.
    proposal.occurred_at = target.occurred_at;
    proposal.expires_at = target.expires_at;

    // Apply the edit to the named field, and keep the *other* field coherent
    // with the target. Injection renders `instruction` as the directive and
    // `content` as the body (§6.2), so a proposal that changed one while
    // dropping the other would inject a half-memory or a contradiction.
    match field {
        EditField::Content => {
            proposal.content = after.clone();
            proposal.instruction = target.instruction.clone();
        }
        EditField::Instruction => {
            proposal.instruction = Some(after.clone());
            // The body carries over verbatim from `Memory::new` above.
        }
    }

    // The gate. These three are what keep the proposal out of every injection
    // until a human decides (§14.8), and they are set explicitly rather than
    // inherited so that a reviewed, identity-verified, corroborated target cannot
    // lend its trust to an agent's rewrite of itself.
    proposal.human_reviewed = false;
    proposal.ai_generated = true;
    proposal.identity_verified = false;
    proposal.corroborating_agents.clear();
    proposal.proposes_supersede_of = Some(target_id.to_string());

    // Saved through the store directly, not through `MemoryWriter`: delta-write
    // would see a near-duplicate of the target and merge or skip it, which is
    // exactly the wrong outcome for something that is meant to *replace* it.
    let saved = store.save(proposal).await?;

    Ok(Proposal {
        proposal_id: saved.id,
        target_id: target_id.to_string(),
        field,
        before,
        after,
        directive_still_says_old,
    })
}

/// Approve a pending memory, applying its proposal if it carries one.
///
/// This is the single place the review flag is flipped together with the
/// supersede, because doing them in either order alone is wrong: flipping the
/// flag without superseding leaves two live near-identical memories, and
/// superseding without the flag leaves the replacement untrusted and therefore
/// uninjected — the target archived and nothing in its place.
///
/// The link is **consumed** (cleared on the approved row) so a second approval
/// cannot re-supersede. The trail survives on the other side — the target's
/// `superseded_by` points at the approved memory — and in `memory_history`, which
/// snapshots the pre-approval row including the link.
///
/// Callers must have already authorized this as an admin action; this function
/// performs no authentication of its own.
pub async fn approve<S: MemoryStore + ?Sized>(store: &S, id: &str) -> Result<ApprovalOutcome> {
    let mut mem = store.get(id).await?;
    mem.human_reviewed = true;
    mem.updated_at = chrono::Utc::now();
    approve_memory(store, mem).await
}

/// Persist an already-modified memory as approved, consuming its proposal link.
///
/// This is the half the "edit then approve" paths need: the Dashboard's inbox
/// edit and MCP `review_memory(action=edit)` let a reviewer correct the text
/// *while* approving, and they must still get the supersede. Calling [`approve`]
/// from those paths instead would mean a second `get`/`update` round trip and a
/// second `memory_history` snapshot for one human decision — noise in the exact
/// audit trail this feature depends on.
pub async fn approve_memory<S: MemoryStore + ?Sized>(
    store: &S,
    mut mem: Memory,
) -> Result<ApprovalOutcome> {
    let id = mem.id.clone();
    let target = mem.proposes_supersede_of.take();

    // The supersede and the approval are ONE storage-layer operation
    // (`MemoryStore::approve_supersede`): a single IMMEDIATE transaction on
    // SQLite, a serialized critical section on Markdown. That closes both
    // audited failure classes the earlier ordered-but-separable two-write
    // shape could not:
    //
    // - a crash between the writes stranded the vault with the target
    //   retired and the proposal still pending — the old rule out of
    //   effect, the new one not in it, until a human noticed and re-ran
    //   the approval (#5);
    // - two concurrent approvals of two proposals for one target could both
    //   read "still unclaimed" and both land, leaving two live
    //   replacements of the same fact (#6).
    //
    // Refusals write nothing at all, so the proposal is left unapproved and
    // reusable, and the retry arm inside the store method (target already
    // points at this proposal) still completes a half-landed legacy state.
    let approved = if let Some(target_id) = &target {
        match store.approve_supersede(target_id, mem).await? {
            ApproveSupersedeOutcome::Approved(approved) => *approved,
            ApproveSupersedeOutcome::TargetGone => {
                return Err(MemVaultError::InvalidInput(format!(
                    "{target_id} no longer exists, so approving {id} cannot supersede it. \
                     Reject the proposal instead; it was left unapproved."
                )));
            }
            ApproveSupersedeOutcome::AlreadyClaimed { by } => {
                return Err(MemVaultError::InvalidInput(format!(
                    "{target_id} was already superseded by {by}. Approving {id} too would \
                     leave two memories both claiming to replace it and neither superseded. \
                     Resolve {by} first, then re-point or reject {id}; it was left unapproved."
                )));
            }
        }
    } else {
        // Ordinary approval with no supersede target: a single row write,
        // atomic in every backend by construction.
        mem.human_reviewed = true;
        mem.updated_at = chrono::Utc::now();
        store.update(mem).await?
    };

    Ok(ApprovalOutcome {
        approved,
        superseded_target: target,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{MemoryType, Priority, Visibility};
    use crate::storage::sqlite::SqliteStore;

    fn agent(id: &str) -> SourceAgent {
        SourceAgent {
            id: id.to_string(),
            agent_type: "coding-assistant".to_string(),
            session_id: Some("sess_proposal".to_string()),
        }
    }

    /// A reviewed, trusted MUST — the strongest form of memory, and therefore the
    /// one a proposal must be least able to affect on its own.
    async fn reviewed_must(store: &SqliteStore, content: &str, instruction: &str) -> Memory {
        let mut mem = Memory::new(
            MemoryType::Preference,
            content.to_string(),
            Priority::Must,
            agent("human"),
        );
        mem.instruction = Some(instruction.to_string());
        mem.human_reviewed = true;
        mem.ai_generated = false;
        mem.identity_verified = true;
        store.save(mem).await.unwrap()
    }

    #[test]
    fn test_edit_field_parse() {
        assert_eq!(EditField::parse("content").unwrap(), EditField::Content);
        assert_eq!(EditField::parse("BODY").unwrap(), EditField::Content);
        assert_eq!(
            EditField::parse("instruction").unwrap(),
            EditField::Instruction
        );
        assert_eq!(
            EditField::parse("directive").unwrap(),
            EditField::Instruction
        );
        assert_eq!(EditField::Content.as_str(), "content");
        assert_eq!(EditField::Instruction.as_str(), "instruction");
        // Unknown must error, not default: defaulting to Content would apply an
        // edit to the wrong field.
        assert!(EditField::parse("tags").is_err());
        assert!(EditField::parse("").is_err());
    }

    #[test]
    fn test_count_occurrences_is_non_overlapping() {
        assert_eq!(
            count_occurrences("aaa", "aa"),
            1,
            "must not double-count overlaps"
        );
        assert_eq!(count_occurrences("a-b-a-b-a", "a"), 3);
        assert_eq!(
            count_occurrences("abc", ""),
            0,
            "empty needle matches nothing"
        );
        assert_eq!(count_occurrences("", "a"), 0);
    }

    #[tokio::test]
    async fn test_propose_creates_a_pending_memory_and_leaves_the_target_alone() {
        let store = SqliteStore::in_memory().unwrap();
        let target = reviewed_must(
            &store,
            "always run migrations before deploy",
            "[MUST] run migrations before deploy",
        )
        .await;

        let proposal = propose_text_edit(
            &store,
            &target.id,
            "before deploy",
            "before every deploy, including hotfixes",
            EditField::Content,
            agent("claude-code"),
        )
        .await
        .unwrap();

        assert_eq!(proposal.target_id, target.id);
        assert_ne!(proposal.proposal_id, target.id);
        assert_eq!(
            proposal.after,
            "always run migrations before every deploy, including hotfixes"
        );

        // The load-bearing assertion: the target is byte-for-byte unchanged and
        // still reviewed. If this failed, any agent could suppress a MUST merely
        // by proposing an edit to it.
        let after = store.get(&target.id).await.unwrap();
        assert_eq!(after.content, "always run migrations before deploy");
        assert!(after.human_reviewed);
        assert!(!after.ai_generated);
        assert!(after.identity_verified);
        assert!(after.superseded_by.is_none());

        // The proposal carries none of the target's trust.
        let pending = store.get(&proposal.proposal_id).await.unwrap();
        assert!(!pending.human_reviewed);
        assert!(pending.ai_generated);
        assert!(!pending.identity_verified);
        assert!(pending.corroborating_agents.is_empty());
        assert_eq!(
            pending.proposes_supersede_of.as_deref(),
            Some(target.id.as_str())
        );
        // …but inherits everything that makes it a replacement rather than a
        // downgrade.
        assert_eq!(pending.priority, Priority::Must);
        assert_eq!(pending.namespace, target.namespace);
    }

    #[tokio::test]
    async fn test_pending_proposal_is_excluded_from_retrieval_but_visible_to_review() {
        let store = SqliteStore::in_memory().unwrap();
        let target = reviewed_must(&store, "the staging database is postgres 14", "use pg14").await;
        let proposal = propose_text_edit(
            &store,
            &target.id,
            "postgres 14",
            "postgres 16",
            EditField::Content,
            agent("claude-code"),
        )
        .await
        .unwrap();

        let found = store
            .search(crate::models::SearchQuery::new(
                "staging database postgres".to_string(),
            ))
            .await
            .unwrap();
        let ids: Vec<&str> = found.results.iter().map(|r| r.memory.id.as_str()).collect();
        assert!(
            ids.contains(&target.id.as_str()),
            "the target must still be retrievable"
        );
        assert!(
            !ids.contains(&proposal.proposal_id.as_str()),
            "a pending proposal is not current truth and must not be returned \
             alongside the memory it would replace"
        );

        // It is not hidden, just not served as truth: the review queue sees it.
        let pending = store.list_pending(None, 50, 0).await.unwrap();
        assert!(pending.iter().any(|m| m.id == proposal.proposal_id));
    }

    #[tokio::test]
    async fn test_approve_consumes_the_link_and_supersedes_the_target() {
        let store = SqliteStore::in_memory().unwrap();
        let target = reviewed_must(&store, "deploy window is Sunday 03:00 UTC", "use Sunday").await;
        let proposal = propose_text_edit(
            &store,
            &target.id,
            "Sunday 03:00 UTC",
            "Saturday 02:00 UTC",
            EditField::Content,
            agent("claude-code"),
        )
        .await
        .unwrap();

        let outcome = approve(&store, &proposal.proposal_id).await.unwrap();
        assert_eq!(
            outcome.superseded_target.as_deref(),
            Some(target.id.as_str())
        );

        let approved = store.get(&proposal.proposal_id).await.unwrap();
        assert!(approved.human_reviewed, "approval must make it injectable");
        assert!(
            approved.proposes_supersede_of.is_none(),
            "the link must be consumed, or a second approve would re-supersede"
        );

        let old = store.get(&target.id).await.unwrap();
        assert_eq!(
            old.superseded_by.as_deref(),
            Some(proposal.proposal_id.as_str())
        );
        assert_eq!(old.layer, crate::models::MemoryLayer::L0);

        // Retrieval now serves the replacement and not the archived original.
        let found = store
            .search(crate::models::SearchQuery::new("deploy window".to_string()))
            .await
            .unwrap();
        let ids: Vec<&str> = found.results.iter().map(|r| r.memory.id.as_str()).collect();
        assert!(ids.contains(&proposal.proposal_id.as_str()));
        assert!(!ids.contains(&target.id.as_str()));

        // A second approval is a no-op on the supersede, not a second supersede.
        let again = approve(&store, &proposal.proposal_id).await.unwrap();
        assert_eq!(again.superseded_target, None);
    }

    #[tokio::test]
    async fn test_approve_of_an_ordinary_memory_reports_no_supersede() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "an ordinary draft".to_string(),
            Priority::Reference,
            agent("claude-code"),
        );
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        let outcome = approve(&store, &id).await.unwrap();
        assert_eq!(outcome.superseded_target, None);
        assert!(store.get(&id).await.unwrap().human_reviewed);
    }

    #[tokio::test]
    async fn test_approve_refuses_a_deleted_target_and_leaves_the_proposal_reusable() {
        // The failure the pre-flight exists for. The old order marked the
        // proposal approved and consumed its link *before* superseding, so a
        // deleted target produced an error AND an injectable memory whose target
        // was never superseded — with no link left to retry through, the only
        // recovery was for a human to notice.
        let store = SqliteStore::in_memory().unwrap();
        let target = reviewed_must(&store, "the staging host is db-1", "use db-1").await;
        let proposal = propose_text_edit(
            &store,
            &target.id,
            "db-1",
            "db-2",
            EditField::Content,
            agent("claude-code"),
        )
        .await
        .unwrap();
        store.delete(&target.id).await.unwrap();

        let err = approve(&store, &proposal.proposal_id).await.unwrap_err();
        assert!(
            matches!(err, MemVaultError::InvalidInput(_)),
            "a deleted target is a refusal, not a storage failure: {err}"
        );
        assert!(
            err.to_string().contains(&target.id),
            "the error has to name the target so a reviewer can act: {err}"
        );

        // Nothing was written. This is the whole point: the proposal is still
        // exactly as approvable-or-not as it was before the attempt.
        let untouched = store.get(&proposal.proposal_id).await.unwrap();
        assert!(
            !untouched.human_reviewed,
            "a refused approval must not make the proposal injectable"
        );
        assert_eq!(
            untouched.proposes_supersede_of.as_deref(),
            Some(target.id.as_str()),
            "the link must survive the refusal, or the proposal cannot be \
             re-pointed or rejected on its own terms"
        );
    }

    #[tokio::test]
    async fn test_second_approval_of_the_same_target_is_refused_not_doubled() {
        // Nothing at propose time forbids two proposals for one target — until
        // either is approved, neither has taken effect. The second *approval* is
        // where it has to be caught: letting it through overwrote the target's
        // `superseded_by` and left the first replacement active and unretired,
        // so two contradictory versions of the same fact were both injectable.
        let store = SqliteStore::in_memory().unwrap();
        let target = reviewed_must(&store, "releases ship on Tuesdays", "ship Tuesday").await;
        let first = propose_text_edit(
            &store,
            &target.id,
            "Tuesdays",
            "Wednesdays",
            EditField::Content,
            agent("claude-code"),
        )
        .await
        .unwrap();
        let second = propose_text_edit(
            &store,
            &target.id,
            "Tuesdays",
            "Thursdays",
            EditField::Content,
            agent("other-agent"),
        )
        .await
        .unwrap();

        approve(&store, &first.proposal_id).await.unwrap();

        let err = approve(&store, &second.proposal_id).await.unwrap_err();
        assert!(
            matches!(err, MemVaultError::InvalidInput(_)),
            "a claimed target is a refusal: {err}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains(&first.proposal_id) && msg.contains(&target.id),
            "the error has to name both the target and the winner so the \
             reviewer knows what to resolve: {msg}"
        );

        // The first supersede is intact — not overwritten.
        assert_eq!(
            store
                .get(&target.id)
                .await
                .unwrap()
                .superseded_by
                .as_deref(),
            Some(first.proposal_id.as_str()),
            "the winning replacement must still be the one on record"
        );
        // The loser is untouched and still a pending proposal.
        let loser = store.get(&second.proposal_id).await.unwrap();
        assert!(
            !loser.human_reviewed,
            "refused approval must not make it live"
        );
        assert_eq!(
            loser.proposes_supersede_of.as_deref(),
            Some(target.id.as_str())
        );

        // Exactly one active version of the fact, which is the invariant that
        // was actually broken.
        let found = store
            .search(crate::models::SearchQuery::new(
                "releases ship on".to_string(),
            ))
            .await
            .unwrap();
        let live: Vec<&str> = found
            .results
            .iter()
            .filter(|r| {
                r.memory.superseded_by.is_none() && r.memory.proposes_supersede_of.is_none()
            })
            .map(|r| r.memory.id.as_str())
            .collect();
        assert_eq!(
            live,
            vec![first.proposal_id.as_str()],
            "one fact, one live version: got {live:?}"
        );
    }

    #[tokio::test]
    async fn test_approve_completes_on_retry_after_a_partial_failure() {
        // The half-landed state: `supersede` got written, the approval did
        // not. On SQLite `approve_supersede` now makes that state unreachable
        // (one IMMEDIATE transaction), but it still exists in the wild —
        // legacy vaults half-approved under the old ordered two-write code,
        // and the markdown backend's two file writes are serialized but not
        // crash-atomic. The retry arm is the recovery path for all of them,
        // and a retry that refuses is worse than the original bug because it
        // removes the only way out. This constructs the state directly and
        // asserts the second attempt finishes the job.
        let store = SqliteStore::in_memory().unwrap();
        let target = reviewed_must(&store, "the retry lock is 30s", "wait 30s").await;
        let proposal = propose_text_edit(
            &store,
            &target.id,
            "30s",
            "45s",
            EditField::Content,
            agent("claude-code"),
        )
        .await
        .unwrap();

        // Simulate: supersede succeeded, approval did not.
        store
            .supersede(&target.id, &proposal.proposal_id)
            .await
            .unwrap();
        assert!(
            !store
                .get(&proposal.proposal_id)
                .await
                .unwrap()
                .human_reviewed
        );

        let outcome = approve(&store, &proposal.proposal_id).await.unwrap();
        assert_eq!(
            outcome.superseded_target.as_deref(),
            Some(target.id.as_str()),
            "the retry must still report what it superseded"
        );
        let approved = store.get(&proposal.proposal_id).await.unwrap();
        assert!(
            approved.human_reviewed,
            "the retry must finish the approval"
        );
        assert!(approved.proposes_supersede_of.is_none(), "link consumed");
        assert_eq!(
            store
                .get(&target.id)
                .await
                .unwrap()
                .superseded_by
                .as_deref(),
            Some(proposal.proposal_id.as_str()),
            "and must not supersede a second time"
        );
    }

    #[tokio::test]
    async fn test_reject_leaves_the_target_untouched() {
        let store = SqliteStore::in_memory().unwrap();
        let target = reviewed_must(&store, "never force-push to main", "no force-push").await;
        let proposal = propose_text_edit(
            &store,
            &target.id,
            "never force-push",
            "sometimes force-push",
            EditField::Content,
            agent("compromised-agent"),
        )
        .await
        .unwrap();

        // Reject is the existing delete path; the assertion that matters is that
        // the target never changed at any point in the proposal's life.
        store.delete(&proposal.proposal_id).await.unwrap();

        let after = store.get(&target.id).await.unwrap();
        assert_eq!(after.content, "never force-push to main");
        assert!(after.superseded_by.is_none());
        assert!(after.human_reviewed);
    }

    #[tokio::test]
    async fn test_ambiguous_old_text_is_refused_rather_than_guessed() {
        let store = SqliteStore::in_memory().unwrap();
        let target = reviewed_must(&store, "use pnpm; pnpm is faster", "use pnpm").await;

        let err = propose_text_edit(
            &store,
            &target.id,
            "pnpm",
            "npm",
            EditField::Content,
            agent("claude-code"),
        )
        .await
        .expect_err("two occurrences must not be resolved by picking one");
        let msg = err.to_string();
        assert!(
            msg.contains("2 times"),
            "error should name the count: {msg}"
        );
        assert!(msg.contains("unambiguous"), "{msg}");

        // Nothing was written.
        assert!(store.list_pending(None, 50, 0).await.unwrap().is_empty());
        assert_eq!(
            store.get(&target.id).await.unwrap().content,
            "use pnpm; pnpm is faster"
        );
    }

    #[tokio::test]
    async fn test_missing_and_noop_edits_are_refused() {
        let store = SqliteStore::in_memory().unwrap();
        let target = reviewed_must(&store, "the retry budget is 3", "retry 3 times").await;

        let absent = propose_text_edit(
            &store,
            &target.id,
            "the retry budget is 5",
            "x",
            EditField::Content,
            agent("claude-code"),
        )
        .await
        .expect_err("old_text is not in the memory");
        assert!(absent.to_string().contains("not found"));

        let noop = propose_text_edit(
            &store,
            &target.id,
            "retry budget",
            "retry budget",
            EditField::Content,
            agent("claude-code"),
        )
        .await
        .expect_err("identical text is not an edit");
        assert!(noop.to_string().contains("identical"));

        let empty = propose_text_edit(
            &store,
            &target.id,
            "",
            "x",
            EditField::Content,
            agent("claude-code"),
        )
        .await
        .expect_err("an empty anchor would match anywhere");
        assert!(empty.to_string().contains("must not be empty"));

        assert!(store.list_pending(None, 50, 0).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_flags_an_edit_that_would_not_change_behaviour() {
        // Found by running the real flow end to end: proposing an edit to
        // `content` on a memory whose `instruction` says the same thing looks
        // successful, approves cleanly, supersedes the target — and changes
        // nothing, because injection renders the directive. The reviewer and the
        // proposing agent both need to be told.
        let store = SqliteStore::in_memory().unwrap();
        let target = reviewed_must(
            &store,
            "always run migrations before deploy",
            "[MUST] run migrations before deploy",
        )
        .await;

        // `before deploy` appears in BOTH fields, so editing only content leaves
        // the directive saying the old thing.
        let flagged = propose_text_edit(
            &store,
            &target.id,
            "before deploy",
            "only after approval",
            EditField::Content,
            agent("claude-code"),
        )
        .await
        .unwrap();
        assert!(
            flagged.directive_still_says_old,
            "must warn: the injected directive still contains the old text"
        );

        // Editing the instruction itself is the fix, and is not flagged.
        let fixed = propose_text_edit(
            &store,
            &target.id,
            "before deploy",
            "only after approval",
            EditField::Instruction,
            agent("claude-code"),
        )
        .await
        .unwrap();
        assert!(!fixed.directive_still_says_old);
    }

    #[tokio::test]
    async fn test_no_false_warning_when_the_directive_differs() {
        let store = SqliteStore::in_memory().unwrap();
        // Instruction does not contain the edited substring, so a content edit
        // genuinely is the whole change — warning here would be noise that trains
        // people to ignore the real one.
        let target =
            reviewed_must(&store, "the staging database is postgres 14", "use staging").await;
        let proposal = propose_text_edit(
            &store,
            &target.id,
            "postgres 14",
            "postgres 16",
            EditField::Content,
            agent("claude-code"),
        )
        .await
        .unwrap();
        assert!(!proposal.directive_still_says_old);
    }

    #[tokio::test]
    async fn test_no_warning_when_there_is_no_directive_at_all() {
        let store = SqliteStore::in_memory().unwrap();
        let mut mem = Memory::new(
            MemoryType::Fact,
            "a body with no directive mentioning deploy".to_string(),
            Priority::Reference,
            agent("human"),
        );
        mem.instruction = None;
        mem.human_reviewed = true;
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        let proposal = propose_text_edit(
            &store,
            &id,
            "no directive",
            "a directive",
            EditField::Content,
            agent("claude-code"),
        )
        .await
        .unwrap();
        assert!(
            !proposal.directive_still_says_old,
            "no instruction means nothing stale to warn about"
        );
    }

    #[tokio::test]
    async fn test_editing_the_instruction_field_targets_the_directive() {
        let store = SqliteStore::in_memory().unwrap();
        let target = reviewed_must(&store, "deploy procedure", "[MUST] require approval").await;

        let proposal = propose_text_edit(
            &store,
            &target.id,
            "require approval",
            "require two approvals",
            EditField::Instruction,
            agent("claude-code"),
        )
        .await
        .unwrap();

        let pending = store.get(&proposal.proposal_id).await.unwrap();
        assert_eq!(
            pending.instruction.as_deref(),
            Some("[MUST] require two approvals"),
            "the directive is what injection renders, so it is what must change"
        );
        assert_eq!(pending.content, "deploy procedure", "the body is untouched");
        // The target's directive is unchanged — this is the exact attack
        // §14.7/§18.3.1 are about, and it must not land without a human.
        assert_eq!(
            store.get(&target.id).await.unwrap().instruction.as_deref(),
            Some("[MUST] require approval")
        );
    }

    #[tokio::test]
    async fn test_editing_a_memory_with_no_instruction_is_refused() {
        let store = SqliteStore::in_memory().unwrap();
        let mut mem = Memory::new(
            MemoryType::Fact,
            "no directive here".to_string(),
            Priority::Reference,
            agent("human"),
        );
        mem.instruction = None;
        mem.human_reviewed = true;
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        let err = propose_text_edit(
            &store,
            &id,
            "no directive",
            "some directive",
            EditField::Instruction,
            agent("claude-code"),
        )
        .await
        .expect_err("there is nothing to edit");
        assert!(err.to_string().contains("no instruction"));
    }

    #[tokio::test]
    async fn test_cannot_propose_an_edit_to_a_superseded_memory() {
        let store = SqliteStore::in_memory().unwrap();
        let old = reviewed_must(&store, "the api base url is v1", "use v1").await;
        let new = reviewed_must(&store, "the api base url is v2", "use v2").await;
        store.supersede(&old.id, &new.id).await.unwrap();

        let err = propose_text_edit(
            &store,
            &old.id,
            "v1",
            "v3",
            EditField::Content,
            agent("claude-code"),
        )
        .await
        .expect_err("archived knowledge is not editable in place");
        assert!(err.to_string().contains("superseded"));
    }

    #[tokio::test]
    async fn test_cannot_chain_a_proposal_onto_a_proposal() {
        let store = SqliteStore::in_memory().unwrap();
        let target = reviewed_must(&store, "the retry budget is 3", "retry 3").await;
        let proposal = propose_text_edit(
            &store,
            &target.id,
            "budget is 3",
            "budget is 5",
            EditField::Content,
            agent("claude-code"),
        )
        .await
        .unwrap();

        let err = propose_text_edit(
            &store,
            &proposal.proposal_id,
            "budget is 5",
            "budget is 7",
            EditField::Content,
            agent("cursor"),
        )
        .await
        .expect_err("chained proposals make 'which version is pending' unanswerable");
        assert!(err.to_string().contains("pending proposal"));
    }

    #[tokio::test]
    async fn test_proposal_inherits_valid_time_so_a_rewrite_cannot_make_it_permanent() {
        let store = SqliteStore::in_memory().unwrap();
        let mut target = Memory::new(
            MemoryType::Fact,
            "the deploy freeze runs until friday".to_string(),
            Priority::Reference,
            agent("human"),
        );
        target.human_reviewed = true;
        let expiry = chrono::Utc::now() + chrono::Duration::days(2);
        target.expires_at = Some(expiry);
        target.occurred_at = Some(chrono::Utc::now() - chrono::Duration::days(9));
        let id = target.id.clone();
        store.save(target).await.unwrap();

        let proposal = propose_text_edit(
            &store,
            &id,
            "until friday",
            "until monday",
            EditField::Content,
            agent("claude-code"),
        )
        .await
        .unwrap();

        let pending = store.get(&proposal.proposal_id).await.unwrap();
        assert_eq!(
            pending.expires_at.map(|d| d.timestamp()),
            Some(expiry.timestamp()),
            "rewording a temporary fact must not quietly make it permanent"
        );
        assert!(pending.occurred_at.is_some(), "event-time carries over too");
    }

    #[tokio::test]
    async fn test_proposal_inherits_visibility_and_namespace() {
        let store = SqliteStore::in_memory().unwrap();
        let mut target = Memory::new(
            MemoryType::Fact,
            "a shared team convention".to_string(),
            Priority::Reference,
            agent("human"),
        );
        target.human_reviewed = true;
        target.visibility = Visibility::Shared;
        target.namespace = "project:memvault".to_string();
        let id = target.id.clone();
        store.save(target).await.unwrap();

        let proposal = propose_text_edit(
            &store,
            &id,
            "shared team convention",
            "shared team rule",
            EditField::Content,
            agent("claude-code"),
        )
        .await
        .unwrap();

        let pending = store.get(&proposal.proposal_id).await.unwrap();
        assert_eq!(pending.visibility, Visibility::Shared);
        assert_eq!(pending.namespace, "project:memvault");

        // A shared proposal must not reach the team pool while pending.
        assert!(
            !store
                .list_shared(20)
                .await
                .unwrap()
                .iter()
                .any(|m| m.id == proposal.proposal_id),
            "an unreviewed proposal must not be injected into every session"
        );
    }

    #[tokio::test]
    async fn test_proposal_survives_the_markdown_backend_roundtrip() {
        // In markdown mode the file IS the source of truth, so a proposal link
        // that did not survive frontmatter would orphan the proposal: approving
        // it would flip the review flag and never supersede the target, silently
        // leaving two live near-identical memories.
        let dir = std::env::temp_dir().join(format!(
            "memvault_proposal_{}",
            uuid::Uuid::new_v4().simple()
        ));
        let store = crate::storage::markdown::MarkdownStore::open(&dir)
            .await
            .unwrap();

        let mut target = Memory::new(
            MemoryType::Fact,
            "markdown backed rule".to_string(),
            Priority::Reference,
            agent("human"),
        );
        target.human_reviewed = true;
        let id = target.id.clone();
        MemoryStore::save(&store, target).await.unwrap();

        let proposal = propose_text_edit(
            &store,
            &id,
            "markdown backed rule",
            "markdown backed convention",
            EditField::Content,
            agent("claude-code"),
        )
        .await
        .unwrap();

        let pending = MemoryStore::get(&store, &proposal.proposal_id)
            .await
            .unwrap();
        assert_eq!(pending.proposes_supersede_of.as_deref(), Some(id.as_str()));
        assert!(!pending.human_reviewed);

        let outcome = approve(&store, &proposal.proposal_id).await.unwrap();
        assert_eq!(outcome.superseded_target.as_deref(), Some(id.as_str()));
        assert_eq!(
            MemoryStore::get(&store, &id)
                .await
                .unwrap()
                .superseded_by
                .as_deref(),
            Some(proposal.proposal_id.as_str())
        );
    }

    /// Audit #6, the actually-concurrent version of
    /// `test_second_approval_of_the_same_target_is_refused_not_doubled`
    /// (which is sequential and therefore only exercises the polite path).
    /// Two proposals for one target are approved *simultaneously* on a
    /// FILE-backed store: the in-memory pool is forced to max_size=1, which
    /// serializes every call and hides the race. The IMMEDIATE transaction
    /// turns read-check-write into a compare-and-swap — exactly one approval
    /// may land, the loser must be refused with nothing written.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_concurrent_approvals_yield_exactly_one_winner() {
        let dir = std::env::temp_dir().join(format!(
            "memvault_approve_race_{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = SqliteStore::new(&dir.join("race.db")).unwrap();

        let target = reviewed_must(&store, "releases ship on Tuesdays", "ship Tuesday").await;
        let first = propose_text_edit(
            &store,
            &target.id,
            "Tuesdays",
            "Wednesdays",
            EditField::Content,
            agent("agent-one"),
        )
        .await
        .unwrap();
        let second = propose_text_edit(
            &store,
            &target.id,
            "Tuesdays",
            "Thursdays",
            EditField::Content,
            agent("agent-two"),
        )
        .await
        .unwrap();

        let (a, b) = tokio::join!(
            approve(&store, &first.proposal_id),
            approve(&store, &second.proposal_id),
        );
        let a_ok = a.is_ok();
        let b_ok = b.is_ok();
        assert!(
            a_ok ^ b_ok,
            "exactly one concurrent approval may land: a_ok={a_ok} b_ok={b_ok}"
        );
        let loser_err = (if a_ok { b } else { a }).unwrap_err().to_string();
        assert!(
            loser_err.contains("already superseded"),
            "the loser must be told why: {loser_err}"
        );

        let winner_id = if a_ok {
            &first.proposal_id
        } else {
            &second.proposal_id
        };
        let loser_id = if a_ok {
            &second.proposal_id
        } else {
            &first.proposal_id
        };
        assert_eq!(
            store
                .get(&target.id)
                .await
                .unwrap()
                .superseded_by
                .as_deref(),
            Some(winner_id.as_str()),
            "the target must point at the winner, not be overwritten by the loser"
        );
        let winner = store.get(winner_id).await.unwrap();
        assert!(winner.human_reviewed);
        assert!(winner.proposes_supersede_of.is_none(), "link consumed");
        let loser = store.get(loser_id).await.unwrap();
        assert!(
            !loser.human_reviewed,
            "the refused approval must not make the loser live"
        );
        assert_eq!(
            loser.proposes_supersede_of.as_deref(),
            Some(target.id.as_str()),
            "the loser stays a pending proposal, reusable after resolution"
        );

        // One fact, one live version — the invariant #6 broke.
        let found = store
            .search(crate::models::SearchQuery::new(
                "releases ship on".to_string(),
            ))
            .await
            .unwrap();
        let live: Vec<&str> = found
            .results
            .iter()
            .filter(|r| {
                r.memory.superseded_by.is_none() && r.memory.proposes_supersede_of.is_none()
            })
            .map(|r| r.memory.id.as_str())
            .collect();
        assert_eq!(live, vec![winner_id.as_str()], "got {live:?}");

        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Audit #5, fault injection at the seam that matters: the SECOND write
    /// inside the atomic operation fails (the proposal row vanished
    /// mid-flight, so its UPDATE matches 0 rows). The target's retirement —
    /// already written inside the same transaction — must roll back with it.
    /// "Old rule retired, nothing approved in its place" is exactly the
    /// stranded state the atomic operation exists to make unreachable.
    #[tokio::test]
    async fn test_failed_approval_write_rolls_back_the_target_retirement() {
        let store = SqliteStore::in_memory().unwrap();
        let target = reviewed_must(&store, "the deploy lock is 30s", "wait 30s").await;
        let proposal = propose_text_edit(
            &store,
            &target.id,
            "30s",
            "45s",
            EditField::Content,
            agent("claude-code"),
        )
        .await
        .unwrap();
        let proposal_mem = store.get(&proposal.proposal_id).await.unwrap();

        // Delete the proposal row behind approve_supersede's back.
        store.delete(&proposal.proposal_id).await.unwrap();

        let err = store
            .approve_supersede(&target.id, proposal_mem)
            .await
            .unwrap_err();
        assert!(
            matches!(err, MemVaultError::NotFound(_)),
            "the missing proposal row must surface as NotFound: {err}"
        );

        let t = store.get(&target.id).await.unwrap();
        assert!(
            t.superseded_by.is_none(),
            "the target's retirement must roll back with the failed approval write"
        );
        assert!(
            t.human_reviewed,
            "the target must be exactly as it was before the attempt"
        );
    }
}
