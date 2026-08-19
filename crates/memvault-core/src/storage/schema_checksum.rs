//! Schema-checksum support for versioned migrations.
//!
//! ## Why checksums, and why TWO of them
//!
//! The invariant a migration checksum guards is "the schema was not quietly
//! changed underneath a migrated database". Comment edits are NOT schema
//! changes — but hashing the raw SQL text makes them indistinguishable, so a
//! checksum computed over raw text breaks every migrated database the moment
//! someone rewords a comment (this has happened twice in the project this
//! design was learned from). Hence:
//!
//! - [`schema_checksum`] — comments stripped **lexically**, whitespace
//!   outside literals collapsed. This is the verdict.
//! - raw text hashing is deliberately NOT stored: it would recreate the
//!   comment-sensitivity bug.
//!
//! ## Why comment stripping must be lexical, not a regex
//!
//! Migration SQL can contain string literals with `--` inside them
//! (`DEFAULT ', -- '` etc.). A `replace("--.*", "")` style regex truncates
//! such literals and produces syntactically invalid SQL — and only fails on
//! migrations that happen to contain such a literal, which looks like random
//! breakage. The scanner below is a small state machine that treats comment
//! markers inside `'…'`, `"…"`, and `[…]` identifiers as ordinary characters.

use sha2::{Digest, Sha256};

/// Bump whenever the normalization rules change, so stored checksums from an
/// older algorithm can be re-baselined instead of being treated as drift.
pub const CHECKSUM_ALGORITHM: i64 = 1;

/// Strip SQL comments lexically. String/identifier literals pass through
/// untouched, including any comment-looking characters inside them.
pub fn strip_sql_comments(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            // '…' literal: copy verbatim, honoring '' escapes
            '\'' => {
                out.push(c);
                loop {
                    match chars.next() {
                        Some('\'') => {
                            out.push('\'');
                            if chars.peek() == Some(&'\'') {
                                out.push(chars.next().unwrap()); // escaped quote
                            } else {
                                break;
                            }
                        }
                        Some(other) => out.push(other),
                        None => break,
                    }
                }
            }
            // "…" identifier: copy verbatim
            '"' => {
                out.push(c);
                for inner in chars.by_ref() {
                    out.push(inner);
                    if inner == '"' {
                        break;
                    }
                }
            }
            // […] identifier: copy verbatim
            '[' => {
                out.push(c);
                for inner in chars.by_ref() {
                    out.push(inner);
                    if inner == ']' {
                        break;
                    }
                }
            }
            // -- line comment (only OUTSIDE literals by construction)
            '-' if chars.peek() == Some(&'-') => {
                chars.next();
                for inner in chars.by_ref() {
                    if inner == '\n' {
                        out.push('\n'); // keep line structure
                        break;
                    }
                }
            }
            // /* block comment */
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = ' ';
                for inner in chars.by_ref() {
                    if prev == '*' && inner == '/' {
                        break;
                    }
                    prev = inner;
                }
                out.push(' '); // a block comment is a separator
            }
            _ => out.push(c),
        }
    }

    out
}

/// Normalize migration SQL for checksumming: strip comments lexically, then
/// collapse whitespace OUTSIDE string literals (whitespace inside a literal
/// is semantically significant and must survive byte-for-byte).
pub fn normalize_for_checksum(sql: &str) -> String {
    let stripped = strip_sql_comments(sql);
    let mut out = String::with_capacity(stripped.len());
    let mut in_single = false;
    let mut prev_space = false;

    for c in stripped.chars() {
        if in_single {
            out.push(c);
            if c == '\'' {
                in_single = false;
            }
            continue;
        }
        match c {
            '\'' => {
                in_single = true;
                prev_space = false;
                out.push(c);
            }
            c if c.is_whitespace() => {
                if !prev_space {
                    out.push(' ');
                    prev_space = true;
                }
            }
            _ => {
                prev_space = false;
                out.push(c);
            }
        }
    }
    out.trim().to_string()
}

/// SHA-256 over the normalized form of a migration statement.
pub fn schema_checksum(sql: &str) -> String {
    let normalized = normalize_for_checksum(sql);
    let digest = Sha256::digest(normalized.as_bytes());
    hex::encode(digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_line_comment() {
        let sql = "ALTER TABLE t ADD COLUMN x TEXT -- adds x\n";
        let stripped = strip_sql_comments(sql);
        assert!(!stripped.contains("adds x"));
        assert!(stripped.contains("ALTER TABLE t ADD COLUMN x TEXT"));
    }

    #[test]
    fn test_strip_block_comment() {
        let sql = "CREATE TABLE t (/* the column */ x TEXT)";
        let stripped = strip_sql_comments(sql);
        assert!(!stripped.contains("the column"));
        assert!(stripped.contains("x TEXT"));
    }

    /// The case that breaks regex-based stripping: `--` inside a string
    /// literal must survive untouched.
    #[test]
    fn test_comment_marker_inside_literal_is_preserved() {
        let sql = "CREATE TABLE t (note TEXT NOT NULL DEFAULT ',     -- ')";
        let stripped = strip_sql_comments(sql);
        assert!(
            stripped.contains("',     -- '"),
            "literal content must survive: {stripped}"
        );
    }

    #[test]
    fn test_escaped_quote_in_literal() {
        let sql = "INSERT INTO t VALUES ('it''s -- not a comment')";
        let stripped = strip_sql_comments(sql);
        assert!(stripped.contains("'it''s -- not a comment'"));
    }

    /// Comment edits are not schema changes: checksum must be stable.
    #[test]
    fn test_checksum_ignores_comments_and_whitespace() {
        let v1 = "ALTER TABLE memories ADD COLUMN x TEXT";
        let v2 = "ALTER TABLE memories\n  ADD COLUMN x TEXT -- new column";
        let v3 = "/* note */ ALTER   TABLE   memories ADD COLUMN x TEXT";
        assert_eq!(schema_checksum(v1), schema_checksum(v2));
        assert_eq!(schema_checksum(v1), schema_checksum(v3));
    }

    /// Semantic changes must move the checksum.
    #[test]
    fn test_checksum_detects_semantic_change() {
        let v1 = "ALTER TABLE memories ADD COLUMN x TEXT";
        let v2 = "ALTER TABLE memories ADD COLUMN x INTEGER";
        assert_ne!(schema_checksum(v1), schema_checksum(v2));
    }

    /// Whitespace inside a literal is semantic and must change the checksum.
    #[test]
    fn test_checksum_sensitive_to_literal_whitespace() {
        let v1 = "CREATE TABLE t (d TEXT DEFAULT 'a, b')";
        let v2 = "CREATE TABLE t (d TEXT DEFAULT 'a,b')";
        assert_ne!(schema_checksum(v1), schema_checksum(v2));
    }
}
