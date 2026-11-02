//! Parsing Markdown documents into chunks, and loading the user's own folder.

use std::path::Path;

/// Longest chunk the assistant receives; longer sections are split at paragraphs.
pub const MAX_WORDS: usize = 320;
const MAX_FILE_BYTES: u64 = 8 << 20;
const MAX_FILES: usize = 2000;

#[derive(Debug, Clone)]
pub struct Doc {
    /// `domain/file`, without the extension.
    pub id: String,
    pub domain: String,
    pub title: String,
    pub tags: String,
    pub aliases: String,
    pub verified: String,
    /// Shipped with the app (reviewed and sourced), as opposed to the user's folder.
    pub builtin: bool,
    /// `(number, text)` of the numbered list under `## Sources`.
    pub sources: Vec<(u32, String)>,
    pub chunks: Vec<Chunk>,
}

#[derive(Debug, Clone)]
pub struct Chunk {
    /// `domain/file#heading-slug`.
    pub id: String,
    pub heading: String,
    pub text: String,
}

impl Doc {
    /// The listed sources that `text` cites as `[1]` or `[1, 2]`.
    pub fn cited(&self, text: &str) -> Vec<&(u32, String)> {
        let nums = citations(text);
        self.sources.iter().filter(|(n, _)| nums.contains(n)).collect()
    }
}

/// Numbers cited in `text` as `[3]`, `[3, 4]` or `[3][4]`.
pub fn citations(text: &str) -> Vec<u32> {
    let mut out = Vec::new();
    for part in text.split('[').skip(1) {
        let Some((inner, _)) = part.split_once(']') else { continue };
        let nums: Option<Vec<u32>> = inner.split(',').map(|n| n.trim().parse().ok()).collect();
        if let Some(nums) = nums {
            out.extend(nums);
        }
    }
    out
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in crate::text::fold(s).chars() {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// Splits frontmatter off; `None` when the file has none.
fn frontmatter(md: &str) -> (Vec<(String, String)>, &str) {
    let rest = md.trim_start_matches('\u{feff}').trim_start();
    let Some(after) = rest.strip_prefix("---") else { return (Vec::new(), rest) };
    let Some(end) = after.find("\n---") else { return (Vec::new(), rest) };
    let fields = after[..end].lines().filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string())).collect();
    let body = after[end + 4..].trim_start_matches(|c| c == '\r' || c == '\n');
    (fields, body)
}

/// Sections by `##` heading (`###` stays inside), ignoring headings in code
/// fences. Text before the first `##` is returned under an empty heading.
fn sections(body: &str) -> Vec<(String, String)> {
    let mut out = vec![(String::new(), String::new())];
    let mut fence = false;
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            fence = !fence;
        }
        if !fence && let Some(h) = line.strip_prefix("## ") {
            out.push((h.trim().to_string(), String::new()));
            continue;
        }
        if !fence && line.starts_with("# ") {
            continue;
        }
        let cur = out.last_mut().unwrap();
        cur.1.push_str(line);
        cur.1.push('\n');
    }
    out
}

/// Splits long text at paragraph breaks, then at words, into parts of at most
/// `MAX_WORDS`. Uses the `target` size to start a new part.
fn split_long(text: &str, target: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut n = 0;
    for para in text.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
        let w = para.split_whitespace().count();
        if w > MAX_WORDS {
            if !cur.is_empty() {
                parts.push(std::mem::take(&mut cur));
                n = 0;
            }
            let all: Vec<&str> = para.split_whitespace().collect();
            parts.extend(all.chunks(target).map(|c| c.join(" ")));
            continue;
        }
        if n + w > target && !cur.is_empty() {
            parts.push(std::mem::take(&mut cur));
            n = 0;
        }
        if !cur.is_empty() {
            cur.push_str("\n\n");
        }
        cur.push_str(para);
        n += w;
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    parts
}

fn parse_sources(text: &str) -> Vec<(u32, String)> {
    text.lines()
        .filter_map(|l| {
            let (n, rest) = l.trim().split_once('.')?;
            Some((n.trim().parse().ok()?, rest.trim().to_string()))
        })
        .collect()
}

/// Parses a document. `path` is relative to the corpus root, e.g. `uwb/foo.md`;
/// its first folder is the domain unless the frontmatter names one.
pub fn parse(path: &str, md: &str, builtin: bool) -> Doc {
    let (fields, body) = frontmatter(md);
    let field = |k: &str| fields.iter().find(|(f, _)| f == k).map(|(_, v)| v.clone()).unwrap_or_default();
    let path = path.replace('\\', "/");
    let stem = path.rsplit_once('.').map_or(path.as_str(), |(s, _)| s).to_string();
    let folder = stem.rsplit_once('/').map(|(d, _)| d.split('/').next().unwrap_or(d).to_string());
    let domain = Some(field("domain")).filter(|d| !d.is_empty()).or(folder).unwrap_or_else(|| "user".to_string()).to_lowercase();
    let file = stem.rsplit('/').next().unwrap_or(&stem);
    let h1 = body.lines().find_map(|l| l.strip_prefix("# ")).map(|t| t.trim().to_string());
    let title = Some(field("title")).filter(|t| !t.is_empty()).or(h1).unwrap_or_else(|| file.replace(['_', '-'], " "));
    let id = format!("{domain}/{}", slug_path(&stem, &domain));

    let mut sources = Vec::new();
    let mut chunks: Vec<Chunk> = Vec::new();
    for (heading, text) in sections(body) {
        if heading.eq_ignore_ascii_case("sources") || heading.eq_ignore_ascii_case("references") {
            sources.extend(parse_sources(&text));
            continue;
        }
        let shown = if heading.is_empty() { title.clone() } else { heading };
        let total = text.split_whitespace().count();
        if total == 0 {
            continue;
        }
        // Keep whole sections when they fit; plain files become even windows.
        let target = if total <= MAX_WORDS { MAX_WORDS } else { 220 };
        let base = slug(&shown);
        for (k, part) in split_long(&text, target).into_iter().enumerate() {
            let mut chunk_id = if k == 0 { format!("{id}#{base}") } else { format!("{id}#{base}-{}", k + 1) };
            let mut dup = 1;
            while chunks.iter().any(|c| c.id == chunk_id) {
                dup += 1;
                chunk_id = format!("{id}#{base}-v{dup}");
            }
            chunks.push(Chunk { id: chunk_id, heading: shown.clone(), text: part });
        }
    }
    Doc { id, domain, title, tags: field("tags"), aliases: field("aliases"), verified: field("verified"), builtin, sources, chunks }
}

/// `uwb/sub/foo` -> `sub/foo` under domain `uwb`; other paths keep all their folders.
fn slug_path(stem: &str, domain: &str) -> String {
    let p = stem.strip_prefix(domain).and_then(|s| s.strip_prefix('/')).unwrap_or(stem);
    p.split('/').map(slug).collect::<Vec<_>>().join("/")
}

#[derive(Debug, Default)]
pub struct LoadReport {
    pub docs: Vec<Doc>,
    /// Files that could not be used, with the reason.
    pub skipped: Vec<(String, String)>,
}

/// Loads `.md` and `.txt` files under `root` (the user's own documents). A
/// missing folder is simply empty.
pub fn load_dir(root: &Path) -> LoadReport {
    let mut report = LoadReport::default();
    let mut stack = vec![root.to_path_buf()];
    let mut seen = 0;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let ext = p.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
            if ext != "md" && ext != "txt" {
                report.skipped.push((rel, format!("{ext} files are not supported; convert to .md or .txt")));
                continue;
            }
            seen += 1;
            if seen > MAX_FILES {
                report.skipped.push((rel, format!("more than {MAX_FILES} files")));
                continue;
            }
            if e.metadata().map(|m| m.len()).unwrap_or(0) > MAX_FILE_BYTES {
                report.skipped.push((rel, "larger than 8 MB".to_string()));
                continue;
            }
            match std::fs::read(&p) {
                Ok(bytes) => {
                    let doc = parse(&rel, &String::from_utf8_lossy(&bytes), false);
                    if doc.chunks.is_empty() {
                        report.skipped.push((rel, "no text".to_string()));
                    } else {
                        report.docs.push(doc);
                    }
                }
                Err(e) => report.skipped.push((rel, e.to_string())),
            }
        }
    }
    report.docs.sort_by(|a, b| a.id.cmp(&b.id));
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    const MD: &str = "---\ntitle: Ranging\ndomain: uwb\ntags: twr, tdoa\nverified: 2026-11-02\n---\n# Ranging\n\nIntro words here.\n\n## DS-TWR clock drift\nDrift is small [1].\n\n```\n## not a heading\n```\n\n## Applying this\nUse rtls_run [2, 3].\n\n## Sources\n1. Paper A — IEEE — http://a\n2. Note B — http://b\n3. Note C — http://c\n";

    #[test]
    fn parses_frontmatter_sections_and_sources() {
        let d = parse("uwb/ranging.md", MD, true);
        assert_eq!(d.id, "uwb/ranging");
        assert_eq!(d.title, "Ranging");
        assert_eq!(d.sources.len(), 3);
        let heads: Vec<_> = d.chunks.iter().map(|c| c.heading.as_str()).collect();
        assert_eq!(heads, ["Ranging", "DS-TWR clock drift", "Applying this"]);
        assert!(d.chunks[1].text.contains("## not a heading"), "fenced heading must not split");
        assert_eq!(d.chunks[1].id, "uwb/ranging#ds-twr-clock-drift");
    }

    #[test]
    fn resolves_only_cited_sources() {
        let d = parse("uwb/ranging.md", MD, true);
        let got: Vec<u32> = d.cited(&d.chunks[2].text).iter().map(|s| s.0).collect();
        assert_eq!(got, [2, 3]);
    }

    #[test]
    fn citation_scan_ignores_non_numbers() {
        assert_eq!(citations("a [1] b [2, 3] [x] [4][5] arr[i, 2]"), [1, 2, 3, 4, 5]);
    }

    #[test]
    fn plain_text_without_frontmatter_becomes_windows() {
        let para = "word ".repeat(100);
        let text = vec![para.trim(); 8].join("\n\n");
        let d = parse("datasheets/dw3000 notes.txt", &text, false);
        assert_eq!(d.domain, "datasheets");
        assert_eq!(d.title, "dw3000 notes");
        assert!(d.chunks.len() > 2);
        assert!(d.chunks.iter().all(|c| c.text.split_whitespace().count() <= MAX_WORDS));
        let mut ids: Vec<_> = d.chunks.iter().map(|c| &c.id).collect();
        ids.dedup();
        assert_eq!(ids.len(), d.chunks.len());
    }

    #[test]
    fn repeated_headings_get_distinct_ids() {
        let d = parse("a/b.md", "## Same\none two\n\n## Same\nthree four\n", false);
        assert_ne!(d.chunks[0].id, d.chunks[1].id);
    }

    #[test]
    fn crlf_and_bom_parse() {
        let d = parse("a/b.md", "\u{feff}---\r\ntitle: T\r\n---\r\n## H\r\nbody text\r\n", false);
        assert_eq!(d.title, "T");
        assert_eq!(d.chunks[0].heading, "H");
    }

    #[test]
    fn loads_folder_and_reports_unsupported() {
        let dir = std::env::temp_dir().join(format!("knowledge_test_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("rf")).unwrap();
        std::fs::write(dir.join("rf/note.md"), "## Loss\nConcrete blocks signal.\n").unwrap();
        std::fs::write(dir.join("a.pdf"), b"%PDF").unwrap();
        std::fs::write(dir.join("empty.txt"), "").unwrap();
        let r = load_dir(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(r.docs.len(), 1);
        assert_eq!(r.docs[0].domain, "rf");
        assert!(!r.docs[0].builtin);
        assert_eq!(r.skipped.len(), 2, "{:?}", r.skipped);
    }

    #[test]
    fn missing_folder_is_empty() {
        let r = load_dir(Path::new("this/does/not/exist"));
        assert!(r.docs.is_empty() && r.skipped.is_empty());
    }
}
