# Knowledge base

Sourced engineering knowledge for the Sense Sim assistant. Everything runs offline: the corpus is compiled into the app and retrieval is BM25 with engineering and Vietnamese synonyms, with no model download.

- `corpus/<domain>/*.md`: the shipped documents. `build.rs` embeds every file here, so adding one needs no code change.
- `src/doc.rs`: Markdown to chunks. Each `##` section is one passage.
- `src/search.rs`, `src/text.rs`: ranking, tokenizer, synonyms.
- `src/api.rs`: the `search_knowledge`, `read_knowledge` and `list_knowledge` tools and automatic attachment.
- `tests/eval.rs`: corpus checks and retrieval evaluation.
- `SIMULATOR_FINDINGS.md`: where simulator defaults disagree with the sources.

## Document format

```
---
title: Short title
domain: uwb
tags: comma, separated, retrieval, keywords
aliases: Vietnamese terms and other wordings
verified: 2026-11-02
---
# Title
## A specific, self-contained section heading
Body, with citations like [1].
## Applying this in Sense Sim
How to use it with real tool names.
## Sources
1. Title — organisation, year — URL
```

Keep sections to 60-250 words and self-contained, since each is read alone. Tests fail on a dangling citation, a missing source or date, or an `rtls_*` / `plant_*` tool that does not exist.

## Test

```
cargo test -p knowledge -- --nocapture --test-threads=1
```

`--nocapture` prints recall for each question set and every miss.
