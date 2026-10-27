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

    /// `"…"` quoted identifiers pass through verbatim, including any
    /// comment-looking characters inside them (SQLite double-quoted
    /// identifiers may legitimately contain `--` or `/*`).
    #[test]
    fn test_double_quoted_identifier_preserves_comment_markers() {
        let sql = r#"CREATE TABLE t ("weird -- col" TEXT)"#;
        let stripped = strip_sql_comments(sql);
        assert!(
            stripped.contains(r#""weird -- col""#),
            "double-quoted identifier must survive verbatim: {stripped}"
        );

        // A block-comment marker inside a double-quoted identifier likewise
        // survives (it is part of the identifier name, not a comment).
        let sql2 = r#"CREATE TABLE t ("a /* b" TEXT)"#;
        let stripped2 = strip_sql_comments(sql2);
        assert!(
            stripped2.contains(r#""a /* b""#),
            "block marker inside identifier must survive: {stripped2}"
        );
    }

    /// `[…]` bracket-quoted identifiers pass through verbatim too.
    #[test]
    fn test_bracket_identifier_preserves_comment_markers() {
        let sql = "CREATE TABLE t ([weird -- col] TEXT)";
        let stripped = strip_sql_comments(sql);
        assert!(
            stripped.contains("[weird -- col]"),
            "bracket identifier must survive verbatim: {stripped}"
        );
    }

    /// A real comment that trails a quoted identifier is still stripped —
    /// guards against the identifier branch over-preserving the whole tail.
    #[test]
    fn test_comment_after_identifier_is_still_stripped() {
        let sql = r#"CREATE TABLE t ("col" TEXT) -- trailing comment"#;
        let stripped = strip_sql_comments(sql);
        assert!(stripped.contains(r#""col""#), "identifier kept: {stripped}");
        assert!(
            !stripped.contains("trailing comment"),
            "real comment must be stripped: {stripped}"
        );
    }

    /// Identifier contents are semantic: two identifiers that differ only by
    /// comment-looking characters inside the quotes must hash differently
    /// (proving the markers are preserved, not stripped, before hashing).
    #[test]
    fn test_checksum_treats_identifier_comment_markers_as_semantic() {
        let v1 = r#"CREATE TABLE t ("a--b" TEXT)"#;
        let v2 = r#"CREATE TABLE t ("ab" TEXT)"#;
        assert_ne!(schema_checksum(v1), schema_checksum(v2));

        let b1 = "CREATE TABLE t ([a--b] TEXT)";
        let b2 = "CREATE TABLE t ([ab] TEXT)";
        assert_ne!(schema_checksum(b1), schema_checksum(b2));
    }

    /// Unterminated literals/identifiers must not panic or hang: the scanner
    /// runs to EOF and returns what it has. (Robustness against a truncated
    /// or hand-edited migration file.)
    #[test]
    fn test_unterminated_literal_and_identifier_do_not_panic() {
        // Unterminated single-quoted string literal → hits the EOF `break`.
        let s1 = strip_sql_comments("INSERT INTO t VALUES ('unclosed");
        assert!(s1.contains("'unclosed"), "got: {s1}");

        // Unterminated double-quoted identifier → drains to EOF.
        let s2 = strip_sql_comments(r#"CREATE TABLE t ("unclosed"#);
        assert!(s2.contains("\"unclosed"), "got: {s2}");

        // Unterminated bracket identifier → drains to EOF.
        let s3 = strip_sql_comments("CREATE TABLE t ([unclosed");
        assert!(s3.contains("[unclosed"), "got: {s3}");
    }
}
