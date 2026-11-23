use crate::model::{Asset, Protocol};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchResult {
    pub score: i32,
    pub name_indices: Vec<usize>,
}

fn is_boundary(ch: char) -> bool {
    matches!(ch, ' ' | '-' | '_' | '.' | '/' | '(' | ':')
}

fn to_lower_chars(s: &str) -> Vec<char> {
    s.chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect()
}

fn fuzzy_match_chars(pat: &[char], text: &str, collect_indices: bool) -> Option<(i32, Vec<usize>)> {
    if pat.is_empty() {
        return Some((0, Vec::new()));
    }

    let hay = to_lower_chars(text);
    if pat.len() > hay.len() {
        return None;
    }

    // Fast path: contiguous substring match gets priority over scattered subsequences.
    for start in 0..=hay.len() - pat.len() {
        if hay[start..start + pat.len()] == pat[..] {
            let mut score = 60 + (pat.len() as i32) * 10;
            if start == 0 {
                score += 35;
            } else if is_boundary(hay[start - 1]) {
                score += 20;
            }
            if hay.len() == pat.len() {
                score += 50;
            }
            let indices = if collect_indices {
                (start..start + pat.len()).collect()
            } else {
                Vec::new()
            };
            return Some((score, indices));
        }
    }

    // Subsequence match with boundary and consecutive bonuses.
    let mut indices = if collect_indices {
        Vec::with_capacity(pat.len())
    } else {
        Vec::new()
    };
    let mut score: i32 = 0;
    let mut pat_idx = 0;
    let mut prev_match: Option<usize> = None;

    for (idx, &ch) in hay.iter().enumerate() {
        if ch == pat[pat_idx] {
            score += 10;
            if idx == 0 || is_boundary(hay[idx - 1]) {
                score += 12;
            }
            if let Some(prev) = prev_match {
                if idx == prev + 1 {
                    score += 15;
                } else {
                    let gap = (idx - prev - 1) as i32;
                    score -= gap.min(15);
                }
            }
            prev_match = Some(idx);
            if collect_indices {
                indices.push(idx);
            }
            pat_idx += 1;
            if pat_idx == pat.len() {
                return Some((score.max(1), indices));
            }
        }
    }

    None
}

#[cfg(test)]
pub fn fuzzy_match_token(token: &str, text: &str) -> Option<(i32, Vec<usize>)> {
    fuzzy_match_chars(&to_lower_chars(token), text, true)
}

fn match_asset_tokens(tokens: &[Vec<char>], asset: &Asset) -> Option<MatchResult> {
    if tokens.is_empty() {
        return Some(MatchResult {
            score: 0,
            name_indices: Vec::new(),
        });
    }

    let mut total_score = 0;
    let mut name_index_set = BTreeSet::new();

    for token in tokens {
        let mut best_token_score: Option<i32> = None;

        if let Some((s, idx)) = fuzzy_match_chars(token, &asset.name, true) {
            best_token_score = Some(best_token_score.map_or(s * 3, |cur| cur.max(s * 3)));
            name_index_set.extend(idx);
        }

        let candidates = [
            (asset.host.as_str(), 3),
            (asset.category.as_str(), 2),
            (asset.protocol.label(), 2),
            (asset.username.as_deref().unwrap_or(""), 1),
            (asset.notes.as_str(), 1),
        ];

        for (field, weight) in candidates {
            if !field.is_empty()
                && let Some((s, _)) = fuzzy_match_chars(token, field, false)
            {
                let weighted = s * weight;
                best_token_score = Some(best_token_score.map_or(weighted, |cur| cur.max(weighted)));
            }
        }

        total_score += best_token_score?;
    }

    Some(MatchResult {
        score: total_score,
        name_indices: name_index_set.into_iter().collect(),
    })
}

pub fn filter_and_sort(
    query: &str,
    protocol_filter: Option<Protocol>,
    assets: &[Asset],
) -> Vec<(usize, MatchResult)> {
    let tokens: Vec<Vec<char>> = query.split_whitespace().map(to_lower_chars).collect();

    let mut matched: Vec<(usize, MatchResult)> = assets
        .iter()
        .enumerate()
        .filter(|(_, asset)| protocol_filter.is_none_or(|p| asset.protocol == p))
        .filter_map(|(idx, asset)| match_asset_tokens(&tokens, asset).map(|res| (idx, res)))
        .collect();

    if !tokens.is_empty() {
        matched.sort_by(|a, b| {
            b.1.score.cmp(&a.1.score).then_with(|| {
                assets[a.0]
                    .name
                    .chars()
                    .flat_map(char::to_lowercase)
                    .cmp(assets[b.0].name.chars().flat_map(char::to_lowercase))
            })
        });
    }

    matched
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::sample_assets;

    #[test]
    fn exact_and_subsequence_matching() {
        let exact = fuzzy_match_token("dc01", "DC01").unwrap();
        let sub = fuzzy_match_token("dc01", "DataCenter-Node-01").unwrap();
        assert!(exact.0 > sub.0);
        assert_eq!(exact.1, vec![0, 1, 2, 3]);
    }

    #[test]
    fn multi_token_thai_and_ip_matching() {
        let assets = sample_assets();
        let thai_hits = filter_and_sort("บัญชี", None, &assets);
        assert_eq!(thai_hits.len(), 1);
        assert!(assets[thai_hits[0].0].name.contains("บัญชี"));

        let multi_hits = filter_and_sort("nas backup", None, &assets);
        assert_eq!(multi_hits.len(), 1);
        assert!(assets[multi_hits[0].0].name.contains("NAS01"));

        let ip_hits = filter_and_sort(".10", None, &assets);
        assert!(!ip_hits.is_empty());
        assert_eq!(assets[ip_hits[0].0].host, "192.168.1.10");

        let miss = filter_and_sort("nonexistent-xyz", None, &assets);
        assert!(miss.is_empty());
    }

    #[test]
    fn protocol_filter_scopes_results() {
        let assets = sample_assets();
        let ssh_only = filter_and_sort("", Some(Protocol::Ssh), &assets);
        assert_eq!(ssh_only.len(), 1);
        assert_eq!(assets[ssh_only[0].0].protocol, Protocol::Ssh);
    }
}
