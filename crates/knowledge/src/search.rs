//! BM25 over weighted fields. Every hit also reports `coverage`, the share of
//! what the query asks for that the chunk mentions, so callers can tell a real
//! answer from a passage that merely shares one common word.

use std::collections::{HashMap, HashSet};

use crate::doc::{Chunk, Doc};
use crate::text::{self, Concept};

const K1: f32 = 1.2;
const B: f32 = 0.75;
const HEADING_WEIGHT: f32 = 3.0;
const TAGS_WEIGHT: f32 = 0.75;
const TITLE_WEIGHT: f32 = 0.75;
/// Weight of an equivalent wording against the words the user wrote.
const SYNONYM_WEIGHT: f32 = 0.85;
/// Each adjacent query pair found adjacent in the chunk.
const PHRASE_BOOST: f32 = 0.15;

struct Entry {
    doc: usize,
    chunk: usize,
    /// Weighted term frequencies over all fields.
    tf: HashMap<String, f32>,
    pairs: HashSet<(String, String)>,
    /// Tokens of the heading, title, tags and aliases: what the chunk is about.
    key: HashSet<String>,
    len: f32,
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub doc: usize,
    pub chunk: usize,
    pub score: f32,
    /// 0..1: IDF-weighted share of the query's concepts present in the chunk.
    pub coverage: f32,
    /// Query concepts named by the chunk's heading, title, tags or aliases, as
    /// opposed to merely appearing in its text.
    pub anchored: usize,
}

pub struct Index {
    pub docs: Vec<Doc>,
    entries: Vec<Entry>,
    df: HashMap<String, u32>,
    avg_len: f32,
}

/// Whether the chunk holds this wording: a word, or a phrase with its words
/// adjacent. Returns the least frequent word and its frequency, for scoring.
fn alt_match<'a>(e: &Entry, alt: &'a [String]) -> Option<(&'a str, f32)> {
    if alt.windows(2).any(|w| !e.pairs.contains(&(w[0].clone(), w[1].clone()))) {
        return None;
    }
    let mut best: Option<(&str, f32)> = None;
    for t in alt {
        let tf = *e.tf.get(t)?;
        if best.is_none_or(|(_, b)| tf < b) {
            best = Some((t, tf));
        }
    }
    best
}

impl Index {
    pub fn build(docs: Vec<Doc>) -> Self {
        let mut entries = Vec::new();
        let mut df: HashMap<String, u32> = HashMap::new();
        for (d, doc) in docs.iter().enumerate() {
            let mut shared = HashMap::new();
            let mut shared_pairs = HashSet::new();
            for (field, w) in [(&doc.title, TITLE_WEIGHT), (&doc.tags, TAGS_WEIGHT), (&doc.aliases, TAGS_WEIGHT)] {
                // Comma-separated keywords: pairs must not cross a comma.
                for part in field.split(',') {
                    let toks = text::tokens(part);
                    shared_pairs.extend(toks.windows(2).map(|p| (p[0].clone(), p[1].clone())));
                    for t in toks {
                        *shared.entry(t).or_insert(0.0) += w;
                    }
                }
            }
            let shared_key: HashSet<String> = shared.keys().cloned().collect();
            for (c, chunk) in doc.chunks.iter().enumerate() {
                let mut tf: HashMap<String, f32> = shared.clone();
                let head = text::tokens(&chunk.heading);
                let body = text::tokens(&chunk.text);
                for t in &head {
                    *tf.entry(t.clone()).or_insert(0.0) += HEADING_WEIGHT;
                }
                for t in &body {
                    *tf.entry(t.clone()).or_insert(0.0) += 1.0;
                }
                let mut pairs = shared_pairs.clone();
                let mut key = shared_key.clone();
                key.extend(head.iter().cloned());
                for seq in [&head, &body] {
                    pairs.extend(seq.windows(2).map(|w| (w[0].clone(), w[1].clone())));
                }
                for t in tf.keys() {
                    *df.entry(t.clone()).or_insert(0) += 1;
                }
                // Length excludes the shared fields, so long chunks are not
                // judged on metadata they all carry.
                let len = body.len() as f32 + HEADING_WEIGHT * head.len() as f32;
                entries.push(Entry { doc: d, chunk: c, tf, pairs, key, len });
            }
        }
        let avg_len = entries.iter().map(|e| e.len).sum::<f32>() / entries.len().max(1) as f32;
        Self { docs, entries, df, avg_len }
    }

    pub fn chunk_count(&self) -> usize {
        self.entries.len()
    }

    fn idf(&self, term: &str) -> f32 {
        let n = self.entries.len() as f32;
        let df = self.df.get(term).copied().unwrap_or(0) as f32;
        (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
    }

    fn weight(&self, e: &Entry, idf: f32, tf: f32) -> f32 {
        let norm = K1 * (1.0 - B + B * e.len / self.avg_len.max(1.0));
        idf * tf * (K1 + 1.0) / (tf + norm)
    }

    /// Best chunks for `query`, optionally within some domains, at most
    /// `per_doc` from one document so results cover different angles.
    pub fn search(&self, query: &str, domains: &[String], limit: usize, per_doc: usize) -> Vec<Hit> {
        let concepts = text::concepts(query);
        if concepts.is_empty() {
            return Vec::new();
        }
        let toks = text::tokens(query);
        let concept_idf = |c: &Concept| c.alts.iter().flatten().map(|t| self.idf(t)).fold(0.0f32, f32::max);
        let total: f32 = concepts.iter().map(concept_idf).sum();
        let wanted: Vec<String> = domains.iter().map(|d| d.trim().to_lowercase()).collect();
        let mut hits = Vec::new();
        for (idx, e) in self.entries.iter().enumerate() {
            if !wanted.is_empty() && !wanted.contains(&self.docs[e.doc].domain) {
                continue;
            }
            let mut score = 0.0;
            let mut matched = 0.0;
            let mut anchored = 0;
            for c in &concepts {
                // All wordings of a concept count as one term: frequencies add
                // up (others discounted) and it is as rare as its commonest wording.
                let mut tf_sum = 0.0f32;
                let mut idf = f32::MAX;
                let mut named = false;
                for (k, alt) in c.alts.iter().enumerate() {
                    let Some((term, tf)) = alt_match(e, alt) else { continue };
                    let w = if k == 0 { 1.0 } else { SYNONYM_WEIGHT };
                    tf_sum += w * tf;
                    idf = idf.min(self.idf(term));
                    named |= alt.iter().all(|t| e.key.contains(t));
                }
                if tf_sum > 0.0 {
                    score += self.weight(e, idf, tf_sum);
                    matched += concept_idf(c);
                    anchored += usize::from(named);
                }
            }
            if score <= 0.0 {
                continue;
            }
            let phrases = toks.windows(2).filter(|w| e.pairs.contains(&(w[0].clone(), w[1].clone()))).count();
            let coverage = if total > 0.0 { matched / total } else { 0.0 };
            let score = score * (0.5 + 0.5 * coverage) * (1.0 + PHRASE_BOOST * phrases as f32);
            hits.push((idx, score, coverage, anchored));
        }
        hits.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let mut taken: HashMap<usize, usize> = HashMap::new();
        let mut out = Vec::new();
        for (idx, score, coverage, anchored) in hits {
            let e = &self.entries[idx];
            let n = taken.entry(e.doc).or_insert(0);
            if *n >= per_doc.max(1) {
                continue;
            }
            *n += 1;
            out.push(Hit { doc: e.doc, chunk: e.chunk, score, coverage, anchored });
            if out.len() >= limit {
                break;
            }
        }
        out
    }

    pub fn chunk(&self, hit: &Hit) -> (&Doc, &Chunk) {
        let d = &self.docs[hit.doc];
        (d, &d.chunks[hit.chunk])
    }

    /// A chunk by `domain/file#section`, or a whole document by `domain/file`.
    pub fn find(&self, id: &str) -> Option<(&Doc, Vec<&Chunk>)> {
        let (doc_id, section) = id.split_once('#').map_or((id, None), |(d, s)| (d, Some(s)));
        let doc = self.docs.iter().find(|d| d.id == doc_id)?;
        match section {
            None => Some((doc, doc.chunks.iter().collect())),
            Some(_) => doc.chunks.iter().find(|c| c.id == id).map(|c| (doc, vec![c])),
        }
    }

    /// `(domain, documents, chunks)`, sorted by domain.
    pub fn domains(&self) -> Vec<(String, usize, usize)> {
        let mut map: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
        for d in &self.docs {
            let e = map.entry(d.domain.clone()).or_default();
            e.0 += 1;
            e.1 += d.chunks.len();
        }
        map.into_iter().map(|(k, (a, b))| (k, a, b)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::parse;

    fn index() -> Index {
        Index::build(vec![
            parse("uwb/twr.md", "---\ntitle: Ranging\ntags: twr\n---\n## DS-TWR clock drift error\nDouble-sided two way ranging cancels most crystal offset.\n\n## Antenna delay calibration\nCalibrate the antenna delay of each device.\n", true),
            parse("rf/walls.md", "---\ntitle: Materials\n---\n## Concrete wall attenuation\nA concrete wall costs many dB at 6.5 GHz.\n\n## Glass loss\nGlass costs little at 2.4 GHz.\n", true),
            parse("rfid/gen2.md", "---\ntitle: Gen2\n---\n## Q algorithm\nThe reader sizes slots with Q to singulate tags.\n", true),
        ])
    }

    fn top(ix: &Index, q: &str) -> String {
        let h = ix.search(q, &[], 3, 2);
        ix.chunk(&h[0]).1.id.clone()
    }

    #[test]
    fn ranks_the_section_that_answers() {
        let ix = index();
        assert_eq!(top(&ix, "clock drift in ds-twr"), "uwb/twr#ds-twr-clock-drift-error");
        assert_eq!(top(&ix, "concrete attenuation"), "rf/walls#concrete-wall-attenuation");
        assert_eq!(top(&ix, "tag singulation Q algorithm"), "rfid/gen2#q-algorithm");
    }

    #[test]
    fn synonyms_and_vietnamese_reach_english_text() {
        let ix = index();
        assert_eq!(top(&ix, "suy hao bê tông"), "rf/walls#concrete-wall-attenuation");
        assert_eq!(top(&ix, "two way ranging"), "uwb/twr#ds-twr-clock-drift-error");
    }

    #[test]
    fn domain_filter_is_respected() {
        let ix = index();
        let h = ix.search("antenna attenuation", &["rf".to_string()], 5, 5);
        assert!(!h.is_empty());
        assert!(h.iter().all(|h| ix.docs[h.doc].domain == "rf"));
    }

    #[test]
    fn nothing_for_stopwords_or_unknown_words() {
        let ix = index();
        assert!(ix.search("what is the", &[], 3, 2).is_empty());
        assert!(ix.search("zzqx flibbert", &[], 3, 2).is_empty());
    }

    #[test]
    fn coverage_separates_partial_from_full_matches() {
        let ix = index();
        let full = ix.search("concrete attenuation", &[], 1, 1);
        let partial = ix.search("concrete zzqx flibbert", &[], 1, 1);
        assert!(full[0].coverage > 0.99, "{}", full[0].coverage);
        assert!(partial[0].coverage < 0.5, "{}", partial[0].coverage);
    }

    #[test]
    fn per_doc_cap_applies() {
        let ix = index();
        let h = ix.search("antenna delay clock drift calibration", &[], 5, 1);
        assert_eq!(h.iter().filter(|h| ix.docs[h.doc].id == "uwb/twr").count(), 1);
    }

    #[test]
    fn find_by_chunk_and_document() {
        let ix = index();
        assert_eq!(ix.find("uwb/twr").unwrap().1.len(), 2);
        assert_eq!(ix.find("uwb/twr#antenna-delay-calibration").unwrap().1.len(), 1);
        assert!(ix.find("uwb/twr#nope").is_none());
        assert!(ix.find("nope").is_none());
    }

    #[test]
    fn empty_index_is_safe() {
        let ix = Index::build(Vec::new());
        assert!(ix.search("anything", &[], 3, 2).is_empty());
        assert_eq!(ix.chunk_count(), 0);
    }
}
