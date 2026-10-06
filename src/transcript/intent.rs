//! Cheap, deterministic signals about an utterance: does it look like a question or request,
//! and does it read as finished? Works on unpunctuated lowercase text (Parakeet) and uses
//! punctuation as a bonus when a provider supplies it (Deepgram). A learned classifier can
//! replace this later behind the same `Assessment`.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Assessment {
    /// 0..=1: how much this looks like something the user should answer.
    pub question: f32,
    /// 0..=1: how much this reads like a finished sentence.
    pub complete: f32,
}

/// Discourse markers that often precede the real question ("so how would you…").
const LEADING_FILLERS: &[&str] = &["so", "okay", "ok", "alright", "and", "um", "uh", "well", "now", "then", "right", "great", "cool", "yeah", "anyway"];
const WH_WORDS: &[&str] = &["what", "why", "how", "when", "where", "who", "which", "whose", "whom"];
const AUXILIARIES: &[&str] = &["can", "could", "would", "will", "should", "do", "does", "did", "is", "are", "was", "were",
    "have", "has", "had", "may", "might", "shall", "isn't", "aren't", "don't", "doesn't", "didn't", "won't", "wouldn't", "can't", "couldn't"];
const REQUESTS: &[&str] = &["tell me", "walk me through", "explain", "describe", "talk me through", "talk about", "give me",
    "what about", "how about", "go over", "show me", "can you", "could you", "would you", "do you"];
/// Words a finished sentence rarely ends on.
const DANGLING: &[&str] = &["and", "or", "but", "the", "a", "an", "of", "to", "for", "with", "in", "on", "at", "from", "like",
    "so", "because", "if", "that", "which", "your", "my", "their", "our", "is", "are", "was", "um", "uh", "than", "about", "into"];

fn normalize(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'').to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

pub fn assess(text: &str) -> Assessment {
    let words = normalize(text);
    if words.is_empty() { return Assessment { question: 0.0, complete: 0.0 }; }
    let trimmed = text.trim_end();
    let body: &[String] = {
        let skip = words.iter().take_while(|w| LEADING_FILLERS.contains(&w.as_str())).count();
        if skip < words.len() { &words[skip..] } else { &words[..] }
    };
    let joined = format!(" {} ", words.join(" "));

    let mut question = 0.0f32;
    if trimmed.ends_with('?') { question += 0.6; }
    if WH_WORDS.contains(&body[0].as_str()) { question += 0.5; }
    if AUXILIARIES.contains(&body[0].as_str()) && body.len() > 1 { question += 0.45; }
    let opening = format!(" {} ", body.join(" "));
    if REQUESTS.iter().any(|phrase| opening.starts_with(&format!(" {phrase} "))) { question += 0.5; }
    else if REQUESTS.iter().any(|phrase| joined.contains(&format!(" {phrase} "))) { question += 0.35; }
    if trimmed.ends_with(", right?") || joined.ends_with(" right ") && words.len() > 3 { question += 0.2; }

    let last = words.last().map(String::as_str).unwrap_or("");
    let complete = if trimmed.ends_with(['?', '.', '!']) { 1.0 }
        else if words.len() < 3 { 0.2 }
        else if DANGLING.contains(&last) { 0.1 }
        else { 0.7 };

    Assessment { question: question.min(1.0), complete }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpunctuated_questions_score_high() {
        for text in ["so how would you design a distributed cache", "would you use redis here", "okay walk me through your approach",
            "what happens when the database changes", "can you explain the trade-offs"] {
            assert!(assess(text).question >= 0.45, "{text}: {:?}", assess(text));
        }
    }

    #[test]
    fn statements_and_backchannels_score_low() {
        for text in ["i worked on the platform team", "yeah", "that makes sense", "we use postgres for everything"] {
            assert!(assess(text).question < 0.3, "{text}: {:?}", assess(text));
        }
    }

    #[test]
    fn punctuation_adds_confidence_when_present() {
        assert!(assess("Would you use Redis here?").question > assess("would you use redis here").question);
        assert_eq!(assess("Sounds good.").complete, 1.0);
    }

    #[test]
    fn dangling_endings_and_fragments_read_as_unfinished() {
        assert!(assess("how would you design a cache and").complete < 0.2);
        assert!(assess("so the").complete < 0.3);
        assert!(assess("how would you design a cache").complete >= 0.7);
        assert_eq!(assess("   ").question, 0.0);
    }
}
