//! FTS5 support: CJK-aware tokenization, tiered query tokens, and safe
//! MATCH-expression construction.
//!
//! ## Why MemVault tokenizes CJK itself
//!
//! SQLite's bundled FTS5 tokenizers cannot index Chinese usefully:
//! `unicode61` does not segment CJK at all (a whole run becomes ONE token,
//! so `MATCH '沙箱'` misses `沙箱环境部署完成了`), and `trigram` needs ≥3
//! characters while two-character words (沙箱/部署/上线) are the most common
//! query shape in Chinese. Verified empirically against the bundled SQLite.
//!
//! The fix is to index CJK runs as **unigrams + adjacent bigrams**:
//! bigrams give two-char-word precision, unigrams keep single-character
//! queries alive. Over-recall from unigrams is acceptable because ranking
//! converges it; *zero recall* cannot be fixed by any ranker.
//!
//! ## Write side and query side must share one tokenizer
//!
//! If indexing tokenizes but the query path forgets (or vice versa), search
//! silently returns nothing — it looks like "no matching memory", not a bug.
//! Everything therefore goes through [`tokenize`].
//!
//! ## MATCH construction is a single entry point
//!
//! FTS5 gives characters like `-`, `OR`, `"`, `*` syntactic meaning. Raw user
//! input reaching MATCH verbatim either errors (`no such column`) or silently
//! changes semantics (prefix queries, boolean ops). [`build_match_expr`] is
//! the only place a MATCH string is built; it quotes every token as a
//! literal.

use crate::error::{MemVaultError, Result};

/// Characters that need unigram+bigram treatment: CJK unified ideographs,
/// extension A, compatibility ideographs, kana, and hangul syllables.
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF   // hiragana + katakana
        | 0x3400..=0x4DBF // CJK extension A
        | 0x4E00..=0x9FFF // CJK unified ideographs
        | 0xF900..=0xFAFF // CJK compatibility ideographs
        | 0xAC00..=0xD7AF // hangul syllables
    )
}

/// ASCII word characters. `+#.-` are kept so identifiers like `k8s-prod` or
/// `c++` survive intact instead of being shredded.
fn is_ascii_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '#' | '.' | '-')
}

/// Tokenize text for FTS5 indexing **and** querying (both sides must use the
/// same function — see module docs).
///
/// CJK runs become unigrams + adjacent bigrams, combined only *within* a run
/// (a bigram spanning punctuation would invent a word the text never
/// contains). ASCII words are kept whole and lowercased; they already have
/// word boundaries, and bigramizing them would make `deploy` match `epl`.
///
/// Tokens are emitted in text order (single positional scan) so future
/// positional operators (`NEAR`, phrases) stay correct.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut cjk_run: Vec<char> = Vec::new();
    let mut ascii_word = String::new();

    // Flush a CJK run: every char as a unigram, plus each adjacent pair.
    let flush_cjk = |run: &mut Vec<char>, out: &mut Vec<String>| {
        for (i, &ch) in run.iter().enumerate() {
            out.push(ch.to_string());
            if let Some(&next) = run.get(i + 1) {
                out.push(format!("{ch}{next}"));
            }
        }
        run.clear();
    };

    for c in text.chars() {
        if is_cjk(c) {
            if !ascii_word.is_empty() {
                tokens.push(ascii_word.to_lowercase());
                ascii_word.clear();
            }
            cjk_run.push(c);
        } else if is_ascii_word(c) {
            if !cjk_run.is_empty() {
                flush_cjk(&mut cjk_run, &mut tokens);
            }
            ascii_word.push(c);
        } else {
            // separator: flush both buffers, do NOT carry a CJK run across it
            if !cjk_run.is_empty() {
                flush_cjk(&mut cjk_run, &mut tokens);
            }
            if !ascii_word.is_empty() {
                tokens.push(ascii_word.to_lowercase());
                ascii_word.clear();
            }
        }
    }
    if !cjk_run.is_empty() {
        flush_cjk(&mut cjk_run, &mut tokens);
    }
    if !ascii_word.is_empty() {
        tokens.push(ascii_word.to_lowercase());
    }

    tokens.dedup();
    tokens
}

/// The recall tiers for a query, tried in order until one returns hits.
///
/// - Tier 0 (strict): full tokenization (unigrams + bigrams), AND-combined.
///   Highest precision.
/// - Tier 1 (relaxed): CJK **unigrams only** + whole ASCII words, AND-combined.
///   The strict tier misses when the query reorders words: querying `部署沙箱`
///   produces the cross-boundary bigram `署沙`, which the document never
///   contains. Dropping bigrams costs some precision but only runs when the
///   strict tier returned nothing — there is no case where it makes results
///   worse. Which tier matched MUST be reported to the caller: silently
///   relaxed matches are not exact matches, and users deserve to know.
///
/// The synonym-OR fallback tier is assembled by the caller (synonyms come
/// from `query_expand`, not from tokenization).
pub fn query_token_tiers(query: &str) -> Vec<Vec<String>> {
    let strict = tokenize(query);

    // Relaxed tier: re-scan keeping only CJK unigrams and whole ASCII words.
    let mut relaxed: Vec<String> = Vec::new();
    let mut ascii_word = String::new();
    for c in query.chars() {
        if is_cjk(c) {
            if !ascii_word.is_empty() {
                relaxed.push(ascii_word.to_lowercase());
                ascii_word.clear();
            }
            relaxed.push(c.to_string());
        } else if is_ascii_word(c) {
            ascii_word.push(c);
        } else if !ascii_word.is_empty() {
            relaxed.push(ascii_word.to_lowercase());
            ascii_word.clear();
        }
    }
    if !ascii_word.is_empty() {
        relaxed.push(ascii_word.to_lowercase());
    }
    relaxed.dedup();

    let mut tiers = vec![strict];
    if !relaxed.is_empty() {
        tiers.push(relaxed);
    }
    tiers.retain(|t| !t.is_empty());
    tiers
}

/// Quote a token as an FTS5 string literal. Double quotes are the only
/// FTS5 escaping mechanism (there is no backslash escape).
fn quote_token(token: &str) -> String {
    format!("\"{}\"", token.replace('"', "\"\""))
}

/// Build an FTS5 MATCH expression from tokens, AND-combined.
///
/// This is the **only** sanctioned way to build a MATCH string in the
/// codebase. AND (not OR): for a tokenized query like `沙 沙箱 箱`, OR would
/// let any memory containing `沙` match and precision would collapse.
pub fn build_match_expr(tokens: &[String]) -> Result<String> {
    let usable: Vec<&String> = tokens.iter().filter(|t| !t.trim().is_empty()).collect();
    if usable.is_empty() {
        return Err(MemVaultError::InvalidInput(
            "empty token list: cannot build FTS5 MATCH expression".to_string(),
        ));
    }
    Ok(usable
        .iter()
        .map(|t| quote_token(t))
        .collect::<Vec<_>>()
        .join(" AND "))
}

/// Build an OR-combined MATCH expression (used for the synonym fallback
/// tier, where ANY of the expanded words matching is a legitimate hit).
pub fn build_match_expr_or(tokens: &[String]) -> Result<String> {
    let usable: Vec<&String> = tokens.iter().filter(|t| !t.trim().is_empty()).collect();
    if usable.is_empty() {
        return Err(MemVaultError::InvalidInput(
            "empty token list: cannot build FTS5 MATCH expression".to_string(),
        ));
    }
    Ok(usable
        .iter()
        .map(|t| quote_token(t))
        .collect::<Vec<_>>()
        .join(" OR "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_cjk_unigrams_and_bigrams() {
        let tokens = tokenize("沙箱环境部署完成了");
        // unigrams + adjacent bigrams, in order
        assert!(tokens.contains(&"沙".to_string()));
        assert!(tokens.contains(&"沙箱".to_string()));
        assert!(tokens.contains(&"箱环".to_string()));
        assert!(tokens.contains(&"环境".to_string()));
        assert!(tokens.contains(&"部署".to_string()));
        assert!(tokens.contains(&"了".to_string()));
        // positional order: '沙' before '沙箱' before '箱'
        let i_sha = tokens.iter().position(|t| t == "沙").unwrap();
        let i_shaxiang = tokens.iter().position(|t| t == "沙箱").unwrap();
        assert!(i_sha < i_shaxiang);
    }

    #[test]
    fn test_tokenize_no_bigram_across_punctuation() {
        let tokens = tokenize("好，的");
        // a bigram 好的 would be a word the source never contained
        assert!(!tokens.contains(&"好的".to_string()));
        assert!(tokens.contains(&"好".to_string()));
        assert!(tokens.contains(&"的".to_string()));
    }

    #[test]
    fn test_tokenize_ascii_words_kept_whole() {
        let tokens = tokenize("Deploy the k8s-prod cluster");
        assert!(tokens.contains(&"deploy".to_string()));
        assert!(tokens.contains(&"k8s-prod".to_string()));
        // bigramizing ASCII would produce noise like 'ep' — must not happen
        assert!(!tokens.contains(&"ep".to_string()));
        assert!(!tokens.contains(&"de".to_string()));
    }

    #[test]
    fn test_tokenize_mixed_cjk_ascii_order_preserved() {
        let tokens = tokenize("修复bug了");
        // ASCII token must sit between the surrounding CJK, not hoisted
        let i_bug = tokens.iter().position(|t| t == "bug").unwrap();
        let i_xiu = tokens.iter().position(|t| t == "修").unwrap();
        let i_le = tokens.iter().position(|t| t == "了").unwrap();
        assert!(i_xiu < i_bug && i_bug < i_le);
    }

    #[test]
    fn test_tokenize_empty_and_whitespace() {
        assert!(tokenize("").is_empty());
        assert!(tokenize("   ").is_empty());
    }

    #[test]
    fn test_query_tiers_strict_then_relaxed() {
        let tiers = query_token_tiers("部署沙箱");
        assert_eq!(tiers.len(), 2);
        // strict tier has the cross-word bigram
        assert!(tiers[0].contains(&"署沙".to_string()));
        // relaxed tier drops CJK bigrams, keeps unigrams
        assert!(!tiers[1].contains(&"署沙".to_string()));
        assert!(tiers[1].contains(&"部".to_string()));
        assert!(tiers[1].contains(&"沙".to_string()));
    }

    #[test]
    fn test_query_tiers_ascii_query() {
        let tiers = query_token_tiers("rust deploy");
        assert!(!tiers.is_empty());
        assert!(tiers[0].contains(&"rust".to_string()));
        assert!(tiers[0].contains(&"deploy".to_string()));
    }

    #[test]
    fn test_build_match_expr_quotes_and_ands() {
        let expr = build_match_expr(&["沙箱".to_string(), "deploy".to_string()]).unwrap();
        assert_eq!(expr, "\"沙箱\" AND \"deploy\"");
    }

    #[test]
    fn test_build_match_expr_escapes_inner_quotes() {
        let expr = build_match_expr(&["a\"b".to_string()]).unwrap();
        assert_eq!(expr, "\"a\"\"b\"");
    }

    #[test]
    fn test_build_match_expr_neutralizes_fts_syntax() {
        // These are the shapes that error or change semantics when passed
        // raw to MATCH; quoted, they are plain literals.
        for evil in ["-沙箱", "x OR y", "NEAR(a b)", "mid:secret", "环*"] {
            let expr = build_match_expr(&[evil.to_string()]).unwrap();
            assert!(expr.starts_with('"') && expr.ends_with('"'));
            assert!(!expr.contains(" OR ") || expr == format!("\"{evil}\""));
        }
    }

    #[test]
    fn test_build_match_expr_empty_errors() {
        assert!(build_match_expr(&[]).is_err());
        assert!(build_match_expr(&["  ".to_string()]).is_err());
        assert!(build_match_expr_or(&[]).is_err());
    }

    #[test]
    fn test_build_match_expr_or() {
        let expr = build_match_expr_or(&["python".to_string(), "js".to_string()]).unwrap();
        assert_eq!(expr, "\"python\" OR \"js\"");
    }
}
