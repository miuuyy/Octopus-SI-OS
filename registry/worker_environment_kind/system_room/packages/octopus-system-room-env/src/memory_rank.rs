//! Ranking of remembered notes for `memory.search`.
//!
//! A note is read as words: maximal runs of Unicode alphanumeric characters,
//! lowercased. A query word that equals a word of the note is an exact match
//! and is scored with BM25 over the visible notes, a title word counting
//! twice. A query word that is only part of a longer word of the note is a
//! partial match. Any exact match ranks above every partial one, so "port"
//! finds the note about a port before the notes about a report or a portal.

use std::cmp::Ordering;
use std::collections::BTreeMap;

const K1: f64 = 1.2;
const B: f64 = 0.75;
const TITLE_BOOST: f64 = 2.0;
const INSIDE_WORD_WEIGHT: f64 = 0.5;
const MIN_PARTIAL_CHARS: usize = 3;

pub(crate) fn words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for character in text.chars() {
        if character.is_alphanumeric() {
            current.extend(character.to_lowercase());
        } else if !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Weighted word counts of one note.
pub(crate) struct Document {
    weights: BTreeMap<String, f64>,
    length: f64,
}

impl Document {
    pub(crate) fn new(title: &str, body: &[&str]) -> Self {
        let mut weights = BTreeMap::new();
        let mut length = 0.0;
        let fields = std::iter::once((title, TITLE_BOOST)).chain(body.iter().map(|text| (*text, 1.0)));
        for (text, boost) in fields {
            for word in words(text) {
                *weights.entry(word).or_insert(0.0) += boost;
                length += boost;
            }
        }
        Self { weights, length }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rank {
    /// BM25 over the query words the note holds as whole words.
    pub exact: f64,
    /// Saturated weight of the query words the note holds only inside longer words.
    pub partial: f64,
}

impl Rank {
    pub(crate) fn matched(&self) -> bool {
        self.exact > 0.0 || self.partial > 0.0
    }

    /// Best first. A whole-word match has a positive BM25 score, so it sorts
    /// before every note that matched only partially.
    pub(crate) fn best_first(&self, other: &Self) -> Ordering {
        other
            .exact
            .total_cmp(&self.exact)
            .then_with(|| other.partial.total_cmp(&self.partial))
    }
}

pub(crate) fn rank(documents: &[Document], terms: &[String]) -> Vec<Rank> {
    let count = documents.len() as f64;
    let total = documents.iter().map(|document| document.length).sum::<f64>();
    let average = if total > 0.0 { total / count } else { 1.0 };
    let inverse_frequency = terms
        .iter()
        .map(|term| {
            let holders = documents
                .iter()
                .filter(|document| document.weights.contains_key(term))
                .count() as f64;
            (1.0 + (count - holders + 0.5) / (holders + 0.5)).ln()
        })
        .collect::<Vec<_>>();
    documents
        .iter()
        .map(|document| {
            let normalization = K1 * (1.0 - B + B * document.length / average);
            let mut rank = Rank { exact: 0.0, partial: 0.0 };
            for (term, inverse_frequency) in terms.iter().zip(&inverse_frequency) {
                if let Some(frequency) = document.weights.get(term) {
                    rank.exact += inverse_frequency * frequency * (K1 + 1.0) / (frequency + normalization);
                    continue;
                }
                if term.chars().count() < MIN_PARTIAL_CHARS {
                    continue;
                }
                let inside = document
                    .weights
                    .iter()
                    .filter(|(word, _)| word.contains(term.as_str()))
                    .map(|(word, weight)| {
                        if word.starts_with(term.as_str()) { *weight } else { weight * INSIDE_WORD_WEIGHT }
                    })
                    .sum::<f64>();
                if inside > 0.0 {
                    rank.partial += inside / (inside + K1);
                }
            }
            rank
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(notes: &[(&str, &str)], query: &str) -> Vec<(usize, Rank)> {
        let documents = notes.iter().map(|(title, body)| Document::new(title, &[body])).collect::<Vec<_>>();
        let mut ranked = rank(&documents, &words(query)).into_iter().enumerate().collect::<Vec<_>>();
        ranked.retain(|(_, rank)| rank.matched());
        ranked.sort_by(|(_, left), (_, right)| left.best_first(right));
        ranked
    }

    #[test]
    fn words_follow_unicode_letters_and_digits() {
        assert_eq!(words("Zażółć gęślą-JAŹŃ: port_8443!"), ["zażółć", "gęślą", "jaźń", "port", "8443"]);
        assert_eq!(words("Порт сервера — 8443"), ["порт", "сервера", "8443"]);
        assert!(words(" \t—…").is_empty());
    }

    #[test]
    fn a_whole_word_outranks_every_longer_word_that_contains_it() {
        let ranked = order(
            &[
                ("Weekly report", "The report report report goes out on Friday, report it."),
                ("Customer portal", "The portal portal portal is the entry point."),
                ("Gateway", "A long note about many other things that mentions the port once near its end."),
            ],
            "port",
        );
        assert_eq!(ranked.iter().map(|(index, _)| *index).collect::<Vec<_>>(), [2, 1, 0]);
        assert!(ranked[0].1.exact > 0.0 && ranked[0].1.partial == 0.0);
        assert!(ranked[1].1.exact == 0.0 && ranked[1].1.partial > ranked[2].1.partial, "a word that starts with the term outranks one that only contains it");
    }

    #[test]
    fn a_rare_word_weighs_more_than_a_common_one_and_a_title_more_than_a_body() {
        let ranked = order(
            &[
                ("Build cache", "octopus build cache location"),
                ("Test runner", "octopus test runner flags"),
                ("Release notes", "octopus release notes format"),
                ("Quorum loss", "recovery after quorum loss"),
                ("Quorum setting", "octopus quorum setting value"),
            ],
            "octopus quorum",
        );
        assert_eq!(ranked[0].0, 4, "the note with both words is first");
        assert_eq!(ranked[1].0, 3, "the rare word alone outranks the common word alone");
        let titled = order(&[("Deploy checklist", "steps to follow"), ("Steps", "the deploy checklist to follow")], "deploy");
        assert_eq!(titled[0].0, 0);
    }
}
