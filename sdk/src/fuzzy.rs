//! Simple match scoring: exact > prefix > word start > initials > substring > subsequence.

pub fn score(name: &str, query: &str) -> Option<i32> {
    let n = name.to_lowercase();
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return None;
    }
    let len_penalty = (n.chars().count() as i32 - q.chars().count() as i32).clamp(0, 50);
    if n == q {
        return Some(1000);
    }
    if n.starts_with(&q) {
        return Some(800 - len_penalty);
    }
    let words: Vec<&str> = n.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    if words.iter().any(|w| w.starts_with(&q)) {
        return Some(650 - len_penalty);
    }
    let initials: String = words.iter().filter_map(|w| w.chars().next()).collect();
    if q.chars().count() >= 2 && initials.starts_with(&q) {
        return Some(600 - len_penalty);
    }
    if n.contains(&q) {
        return Some(450 - len_penalty);
    }
    // Every query word must appear somewhere: "code vis" -> "Visual Studio Code".
    let qwords: Vec<&str> = q.split_whitespace().collect();
    if qwords.len() > 1 && qwords.iter().all(|qw| words.iter().any(|w| w.starts_with(qw))) {
        return Some(500 - len_penalty);
    }
    // Subsequence, rewarding consecutive runs.
    let mut it = n.chars().peekable();
    let mut run = 0;
    let mut bonus = 0;
    for qc in q.chars().filter(|c| !c.is_whitespace()) {
        let mut found = false;
        let mut first = true;
        while let Some(c) = it.next() {
            if c == qc {
                run = if first { run + 1 } else { 1 };
                bonus += run;
                found = true;
                break;
            }
            first = false;
        }
        if !found {
            return None;
        }
    }
    Some(100 + bonus * 5 - len_penalty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranking() {
        let s = |n| score(n, "vs").unwrap_or(-1);
        assert!(s("Visual Studio Code") > s("Avast Secure"));
        assert!(score("Telegram", "tel").unwrap() > score("Hotel Manager", "tel").unwrap());
        assert!(score("Steam", "xyz").is_none());
        assert!(score("Visual Studio Code", "code vis").is_some());
    }
}
