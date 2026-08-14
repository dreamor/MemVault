use std::collections::HashMap;

static SYNONYMS: &[(&str, &[&str])] = &[
    ("js", &["javascript", "node", "nodejs"]),
    ("javascript", &["js", "node", "nodejs"]),
    ("ts", &["typescript"]),
    ("typescript", &["ts"]),
    ("python", &["py", "python3"]),
    ("py", &["python", "python3"]),
    ("rust", &["cargo", "rustlang"]),
    ("go", &["golang"]),
    ("cpp", &["c++", "cplusplus"]),
    ("c++", &["cpp", "cplusplus"]),
    ("db", &["database", "sql"]),
    ("database", &["db", "sql"]),
    ("sql", &["database", "db", "query"]),
    ("api", &["endpoint", "rest", "http", "接口"]),
    ("endpoint", &["api", "route"]),
    ("frontend", &["front-end", "ui", "前端"]),
    ("backend", &["back-end", "server", "后端"]),
    ("test", &["testing", "spec", "测试"]),
    ("testing", &["test", "spec"]),
    ("deploy", &["deployment", "release", "部署"]),
    ("docker", &["container", "容器"]),
    ("k8s", &["kubernetes"]),
    ("kubernetes", &["k8s"]),
    ("ci", &["ci/cd", "pipeline"]),
    ("git", &["version control", "vcs"]),
    ("pr", &["pull request", "merge request", "mr"]),
    ("code", &["coding", "programming", "编程", "代码"]),
    ("coding", &["code", "programming", "编程"]),
    ("编程", &["code", "coding", "代码"]),
    ("代码", &["code", "coding", "编程"]),
    ("函数", &["function", "方法"]),
    ("function", &["函数", "method", "func"]),
    ("写作", &["writing", "文章"]),
    ("writing", &["写作", "文章", "write"]),
    ("设计", &["design", "架构"]),
    ("design", &["设计", "architecture"]),
    ("项目", &["project", "工程"]),
    ("project", &["项目", "工程"]),
    ("偏好", &["preference", "喜欢", "prefer"]),
    ("prefer", &["偏好", "preference", "like"]),
    ("style", &["风格", "格式", "format"]),
    ("风格", &["style", "格式"]),
    ("简洁", &["concise", "简短", "brief"]),
    ("注释", &["comment", "comments"]),
    ("comment", &["注释", "comments"]),
];

pub fn expand_query(query: &str) -> Vec<String> {
    let words = tokenize(query);
    let mut expanded: Vec<String> = words.clone();

    let map = synonym_map();

    // match single tokens
    for word in &words {
        let lower = word.to_lowercase();
        if let Some(syns) = map.get(lower.as_str()) {
            for s in *syns {
                let s_str = s.to_string();
                if !expanded.contains(&s_str) {
                    expanded.push(s_str);
                }
            }
        }
    }

    // For multi-char CJK keys (no spaces → no word boundaries), match against
    // the full query. ASCII keys are intentionally excluded here: a substring
    // match would expand "prefer" (contains "pr"), "google" (contains "go") or
    // "contest" (contains "ts") into irrelevant synonyms on every search.
    let full_lower = query.to_lowercase();
    for (key, syns) in SYNONYMS {
        if !key.is_ascii() && full_lower.contains(key) {
            for s in *syns {
                let s_str = s.to_string();
                if !expanded.contains(&s_str) {
                    expanded.push(s_str);
                }
            }
        }
    }

    expanded
}

pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();

    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '+' {
            current.push(ch);
        } else if is_cjk(ch) {
            // flush ASCII token
            if current.len() > 1 {
                tokens.push(current.clone());
            }
            current.clear();
            // each CJK char or bigram is a token
            tokens.push(ch.to_string());
        } else {
            // separator
            if current.len() > 1 {
                tokens.push(current.clone());
            }
            current.clear();
        }
    }
    if current.len() > 1 {
        tokens.push(current);
    }

    // also produce CJK bigrams for better matching
    let cjk_chars: Vec<char> = text.chars().filter(|c| is_cjk(*c)).collect();
    for window in cjk_chars.windows(2) {
        tokens.push(window.iter().collect());
    }

    tokens.sort();
    tokens.dedup();
    tokens
}

fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}' |
        '\u{3400}'..='\u{4DBF}' |
        '\u{F900}'..='\u{FAFF}'
    )
}

fn synonym_map() -> HashMap<&'static str, &'static [&'static str]> {
    SYNONYMS.iter().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_python() {
        let expanded = expand_query("Python");
        assert!(expanded.contains(&"Python".to_string()));
        assert!(expanded.iter().any(|w| w == "py" || w == "python3"));
    }

    #[test]
    fn test_expand_js() {
        let expanded = expand_query("JS framework");
        assert!(
            expanded
                .iter()
                .any(|w| w.to_lowercase() == "javascript" || w.to_lowercase() == "node")
        );
    }

    #[test]
    fn test_expand_chinese() {
        let expanded = expand_query("编程偏好");
        assert!(expanded.iter().any(|w| w == "code" || w == "coding"));
        assert!(expanded.iter().any(|w| w == "preference" || w == "prefer"));
    }

    #[test]
    fn test_no_expansion() {
        let expanded = expand_query("hello world");
        assert_eq!(expanded.len(), 2); // no synonyms for these
    }

    /// Regression: the full-query substring pass used to match short ASCII keys
    /// anywhere in the string — "prefer"/"programming" contain "pr", "google"
    /// contains "go", "contest" contains "ts"/"test" — injecting junk synonyms
    /// into every search. ASCII keys must only match as whole tokens.
    #[test]
    fn test_expand_ascii_keys_require_token_boundary() {
        let expanded = expand_query("I prefer Python programming");
        assert!(
            !expanded
                .iter()
                .any(|w| w.eq_ignore_ascii_case("merge request") || w == "mr")
        );
        assert!(!expanded.iter().any(|w| w == "pull request"));

        let expanded_go = expand_query("google cloud");
        assert!(!expanded_go.iter().any(|w| w == "golang"));

        let expanded_test = expand_query("latest contest");
        assert!(!expanded_test.iter().any(|w| w == "testing" || w == "spec"));
    }

    /// Whole-token matches for short keys must still work.
    #[test]
    fn test_expand_ascii_key_as_whole_token() {
        let expanded = expand_query("use PR for reviews");
        assert!(expanded.iter().any(|w| w == "mr"));
        let expanded_go = expand_query("go is fast");
        assert!(expanded_go.iter().any(|w| w == "golang"));
    }

    #[test]
    fn test_tokenize() {
        let tokens = tokenize("Python API, database/sql");
        assert!(tokens.contains(&"Python".to_string()));
        assert!(tokens.contains(&"API".to_string()));
        assert!(tokens.contains(&"database".to_string()));
        assert!(tokens.contains(&"sql".to_string()));
    }
}
