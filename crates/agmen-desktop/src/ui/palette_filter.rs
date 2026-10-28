//! Fuzzy filter for the command palette, with no GPUI.
//!
//! A label matches when the query's characters appear in it in order, ignoring
//! case and the query's whitespace. A small dynamic program picks the best
//! alignment: matches at word starts and in consecutive runs score higher, and
//! among matches in the first few characters, an earlier start ranks higher.

use std::ops::Range;

/// Query characters past this many are ignored, which bounds the work per
/// label at `MAX_QUERY_CHARS * MAX_LABEL_CHARS` cells.
pub(crate) const MAX_QUERY_CHARS: usize = 256;
/// Label characters past this many never match.
pub(crate) const MAX_LABEL_CHARS: usize = 512;
/// `filter` returns at most this many matches, the best first.
pub(crate) const MAX_RESULTS: usize = 500;

const BASE: i64 = 1;
const WORD_START: i64 = 10;
const CONSECUTIVE: i64 = 8;
const GAP: i64 = 3;
/// The start offset costs at most this much, so a word start late in a label
/// still beats a mid-word match near its start.
const MAX_START_PENALTY: usize = 3;
/// Marks a cell where the query prefix cannot end.
const NONE: i64 = i64::MIN / 2;

/// One matching label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Match {
    /// Position of the label in the slice given to `filter`.
    pub(crate) index: usize,
    /// Higher is better.
    pub(crate) score: i64,
    /// Byte ranges of the matched characters in the label, merged into runs,
    /// in order. Each lands on UTF-8 boundaries. Empty for an empty query.
    pub(crate) ranges: Vec<Range<usize>>,
}

/// Returns the labels that match `query`, best first, at most `MAX_RESULTS`.
/// Equal scores keep input order. An empty or all-whitespace query returns
/// every label in input order.
#[must_use]
pub(crate) fn filter<S: AsRef<str>>(query: &str, labels: &[S]) -> Vec<Match> {
    let query: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .take(MAX_QUERY_CHARS)
        .map(fold)
        .collect();
    let mut scratch = Scratch::default();
    let mut matches: Vec<Match> = labels
        .iter()
        .enumerate()
        .filter_map(|(index, label)| {
            let (score, ranges) = score(&query, label.as_ref(), &mut scratch)?;
            Some(Match {
                index,
                score,
                ranges,
            })
        })
        .collect();
    // A stable sort keeps input order among equal scores.
    matches.sort_by_key(|m| std::cmp::Reverse(m.score));
    matches.truncate(MAX_RESULTS);
    matches
}

/// Buffers reused across labels, so a keystroke allocates per match, not per
/// label.
#[derive(Debug, Default)]
struct Scratch {
    chars: Vec<(usize, char)>,
    best: Vec<i64>,
    from: Vec<usize>,
}

/// Scores the best alignment of `query` (already folded) in `label`.
fn score(query: &[char], label: &str, scratch: &mut Scratch) -> Option<(i64, Vec<Range<usize>>)> {
    if query.is_empty() {
        return Some((0, Vec::new()));
    }
    let Scratch { chars, best, from } = scratch;
    chars.clear();
    chars.extend(label.char_indices().take(MAX_LABEL_CHARS));
    let (m, n) = (query.len(), chars.len());
    if m > n {
        return None;
    }
    // best[i * n + j]: best score with query[i] matched at chars[j].
    // from[i * n + j]: where query[i - 1] matched on that path.
    best.clear();
    best.resize(m * n, NONE);
    from.clear();
    from.resize(m * n, 0);
    for (i, &q) in query.iter().enumerate() {
        // Best cell in row i - 1 at or before j - 2, for a gapped step.
        let mut gapped: Option<(i64, usize)> = None;
        for j in i..n {
            let (_, c) = chars[j];
            if i > 0 && j >= 2 {
                let prev = best[(i - 1) * n + j - 2];
                if prev > NONE && gapped.is_none_or(|(s, _)| prev > s) {
                    gapped = Some((prev, j - 2));
                }
            }
            if fold(c) != q {
                continue;
            }
            let bonus = BASE
                + if is_word_start(chars, j) {
                    WORD_START
                } else {
                    0
                };
            if i == 0 {
                best[j] = bonus - i64::try_from(j.min(MAX_START_PENALTY)).unwrap_or(0);
                continue;
            }
            let run = best[(i - 1) * n + j - 1];
            let run = (run > NONE).then_some((run + CONSECUTIVE, j - 1));
            let gap = gapped.map(|(s, k)| (s - GAP, k));
            // On a tie, prefer the consecutive run.
            let step = match (run, gap) {
                (Some(r), Some(g)) => Some(if g.0 > r.0 { g } else { r }),
                (r, g) => r.or(g),
            };
            if let Some((s, k)) = step {
                best[i * n + j] = s + bonus;
                from[i * n + j] = k;
            }
        }
    }
    let last = &best[(m - 1) * n..];
    // The earliest end wins a tie, so earlier matches beat later ones.
    let (mut j, &top) = last
        .iter()
        .enumerate()
        .rev()
        .max_by_key(|&(_, &s)| s)
        .filter(|&(_, &s)| s > NONE)?;
    let mut hits = vec![0_usize; m];
    for i in (0..m).rev() {
        hits[i] = j;
        j = from[i * n + j];
    }
    Some((top, ranges(chars, &hits)))
}

/// Merges the matched char positions into byte ranges.
fn ranges(chars: &[(usize, char)], hits: &[usize]) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::with_capacity(hits.len());
    for &j in hits {
        let (start, c) = chars[j];
        let end = start + c.len_utf8();
        match out.last_mut() {
            Some(r) if r.end == start => r.end = end,
            _ => out.push(start..end),
        }
    }
    out
}

/// A word starts at the label's start, after a non-alphanumeric character,
/// or at an uppercase letter after a lowercase one.
fn is_word_start(chars: &[(usize, char)], j: usize) -> bool {
    let Some(&(_, prev)) = j.checked_sub(1).and_then(|p| chars.get(p)) else {
        return true;
    };
    let c = chars[j].1;
    !prev.is_alphanumeric() || (prev.is_lowercase() && c.is_uppercase())
}

/// Lowercases `c` when that gives one character, so offsets stay per char.
fn fold(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

#[cfg(test)]
#[path = "palette_filter_tests.rs"]
mod tests;
