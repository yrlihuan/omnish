use serde::{Deserialize, Serialize};

/// A pending sample buffered in the daemon session, waiting for the next command.
#[derive(Debug, Clone)]
pub struct PendingSample {
    pub session_id: String,
    pub context: String,
    pub prompt: String,
    pub suggestions: Vec<String>,
    pub input: String,
    pub cwd: Option<String>,
    pub latency_ms: u64,
    pub accepted: bool,
}

/// A completed sample record written to JSONL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionSample {
    pub recorded_at: String,
    pub session_id: String,
    pub context: String,
    pub prompt: String,
    pub suggestions: Vec<String>,
    pub input: String,
    pub accepted: bool,
    pub next_command: Option<String>,
    pub similarity: Option<f64>,
    pub cwd: Option<String>,
    pub latency_ms: u64,
}

/// Levenshtein edit distance between two strings.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (m, n) = (a.len(), b.len());
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0; n + 1];
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1)
                .min(curr[j - 1] + 1)
                .min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

/// Similarity ratio (0.0 = completely different, 1.0 = identical).
pub fn similarity(a: &str, b: &str) -> f64 {
    let max_len = a.chars().count().max(b.chars().count());
    if max_len == 0 {
        return 1.0;
    }
    1.0 - (levenshtein(a, b) as f64 / max_len as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_levenshtein_identical() {
        assert_eq!(levenshtein("git status", "git status"), 0);
    }

    #[test]
    fn test_levenshtein_one_edit() {
        assert_eq!(levenshtein("git status", "git statu"), 1);
    }

    #[test]
    fn test_levenshtein_empty() {
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("", ""), 0);
    }

    #[test]
    fn test_similarity_identical() {
        let s = similarity("git status", "git status");
        assert!((s - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_similarity_near_miss() {
        let s = similarity("git status --short", "git status -s");
        assert!(s > 0.3 && s < 1.0);
    }

    #[test]
    fn test_similarity_completely_different() {
        let s = similarity("ls -la", "docker compose up");
        assert!(s < 0.3);
    }

    #[test]
    fn test_similarity_empty() {
        assert!((similarity("", "") - 1.0).abs() < f64::EPSILON);
        assert!((similarity("abc", "")).abs() < f64::EPSILON);
    }
}
