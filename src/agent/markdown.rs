//! Minimal Markdown rendering for chat messages: headings, bullet and
//! numbered lists, block quotes, fenced code, rules, and inline **bold**,
//! *italic*, `code` and [links](url).

use bevy_egui::egui::{self, Color32, FontId, Margin, RichText, Stroke, TextFormat, text::LayoutJob, vec2};

use crate::theme;

pub fn show(ui: &mut egui::Ui, text: &str) {
    let mut lines = text.lines().peekable();
    let mut para = String::new();
    let flush = |ui: &mut egui::Ui, para: &mut String| {
        if !para.trim().is_empty() {
            inline(ui, para.trim(), 14.0, theme::TEXT, false);
        }
        para.clear();
    };
    while let Some(line) = lines.next() {
        let t = line.trim_end();
        let trimmed = t.trim_start();
        if let Some(lang) = trimmed.strip_prefix("```") {
            flush(ui, &mut para);
            let _ = lang;
            let mut code = String::new();
            for l in lines.by_ref() {
                if l.trim_start().starts_with("```") {
                    break;
                }
                code.push_str(l);
                code.push('\n');
            }
            egui::Frame::new().fill(theme::BG).corner_radius(6).inner_margin(Margin::same(8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add(egui::Label::new(RichText::new(code.trim_end()).monospace().size(12.5).color(theme::TEXT)).wrap());
            });
            continue;
        }
        if trimmed.is_empty() {
            flush(ui, &mut para);
            ui.add_space(4.0);
            continue;
        }
        if trimmed.chars().all(|c| c == '-' || c == '*' || c == '_') && trimmed.len() >= 3 {
            flush(ui, &mut para);
            theme::divider(ui);
            continue;
        }
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
            flush(ui, &mut para);
            let size = match hashes {
                1 => 19.0,
                2 => 17.0,
                _ => 15.0,
            };
            ui.add_space(2.0);
            inline(ui, trimmed[hashes..].trim(), size, theme::TEXT, true);
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("> ") {
            flush(ui, &mut para);
            egui::Frame::new()
                .stroke(Stroke::NONE)
                .inner_margin(Margin { left: 10, right: 0, top: 0, bottom: 0 })
                .show(ui, |ui| inline(ui, rest, 14.0, theme::WEAK, false));
            continue;
        }
        if let Some((marker, rest)) = list_item(trimmed) {
            flush(ui, &mut para);
            let indent = (t.len() - trimmed.len()) as f32 * 6.0;
            // Marker in a fixed column, top-aligned with the first line, so
            // wrapped lines hang under the text rather than the bullet.
            ui.horizontal_top(|ui| {
                ui.add_space(indent);
                ui.spacing_mut().item_spacing.x = 4.0;
                let w = if marker == "•" { 14.0 } else { 20.0 };
                ui.add_sized(vec2(w, 18.0), egui::Label::new(RichText::new(&marker).size(14.0).color(theme::WEAK)));
                ui.vertical(|ui| inline(ui, rest, 14.0, theme::TEXT, false));
            });
            continue;
        }
        if !para.is_empty() {
            para.push(' ');
        }
        para.push_str(trimmed);
    }
    flush(ui, &mut para);
}

/// "- x", "* x", "+ x" or "12. x" -> (bullet, rest).
fn list_item(s: &str) -> Option<(String, &str)> {
    for m in ["- ", "* ", "+ "] {
        if let Some(rest) = s.strip_prefix(m) {
            return Some(("•".into(), rest));
        }
    }
    let digits = s.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && digits < 4 && s[digits..].starts_with(". ") {
        return Some((format!("{}.", &s[..digits]), &s[digits + 2..]));
    }
    None
}

/// Inline spans: **bold**, *italic* / _italic_, `code`, [text](url).
fn inline(ui: &mut egui::Ui, s: &str, size: f32, color: Color32, heading: bool) {
    let mut job = LayoutJob::default();
    job.wrap.max_width = ui.available_width();
    let base = |bold: bool, italic: bool| {
        let mut f = TextFormat {
            font_id: if bold || heading { FontId::new(size, egui::FontFamily::Name(theme::SEMIBOLD.into())) } else { FontId::proportional(size) },
            color: if bold || heading { Color32::WHITE } else { color },
            ..Default::default()
        };
        f.italics = italic;
        f
    };
    let code_fmt = TextFormat {
        font_id: FontId::monospace(size - 1.5),
        color: theme::ACCENT,
        background: theme::BG,
        ..Default::default()
    };
    let link_fmt = TextFormat { font_id: FontId::proportional(size), color: theme::ONLINE, underline: Stroke::new(1.0, theme::ONLINE), ..Default::default() };

    let chars: Vec<char> = s.chars().collect();
    let (mut bold, mut italic) = (false, false);
    let mut buf = String::new();
    let mut i = 0;
    let push = |job: &mut LayoutJob, buf: &mut String, bold: bool, italic: bool| {
        if !buf.is_empty() {
            job.append(buf, 0.0, base(bold, italic));
            buf.clear();
        }
    };
    let mut links = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        if c == '`'
            && let Some(end) = chars[i + 1..].iter().position(|&x| x == '`')
        {
            push(&mut job, &mut buf, bold, italic);
            let code: String = chars[i + 1..i + 1 + end].iter().collect();
            job.append(&format!("\u{2009}{code}\u{2009}"), 0.0, code_fmt.clone());
            i += end + 2;
            continue;
        }
        if c == '*' && chars.get(i + 1) == Some(&'*') {
            push(&mut job, &mut buf, bold, italic);
            bold = !bold;
            i += 2;
            continue;
        }
        if (c == '*' || c == '_') && is_emphasis_marker(&chars, i) {
            push(&mut job, &mut buf, bold, italic);
            italic = !italic;
            i += 1;
            continue;
        }
        if c == '['
            && let Some(close) = chars[i..].iter().position(|&x| x == ']')
            && chars.get(i + close + 1) == Some(&'(')
            && let Some(end) = chars[i + close + 2..].iter().position(|&x| x == ')')
        {
            push(&mut job, &mut buf, bold, italic);
            let text: String = chars[i + 1..i + close].iter().collect();
            let url: String = chars[i + close + 2..i + close + 2 + end].iter().collect();
            job.append(&text, 0.0, link_fmt.clone());
            links.push(url);
            i += close + 3 + end;
            continue;
        }
        buf.push(c);
        i += 1;
    }
    push(&mut job, &mut buf, bold, italic);
    let r = ui.add(egui::Label::new(job).wrap());
    if !links.is_empty() {
        let r = r.on_hover_text(links.join("\n"));
        if r.clicked() {
            ui.ctx().open_url(egui::OpenUrl::new_tab(&links[0]));
        }
    }
}

/// A lone `*` or `_` that opens or closes emphasis (not inside a word such as
/// `snake_case` or `2*3`).
fn is_emphasis_marker(chars: &[char], i: usize) -> bool {
    let prev = i.checked_sub(1).map(|p| chars[p]);
    let next = chars.get(i + 1).copied();
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric());
    match (word(prev), word(next)) {
        (false, true) => true,  // opening
        (true, false) => true,  // closing
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_items() {
        assert_eq!(list_item("- **Tầng dưới:** 2 lớp").unwrap().1, "**Tầng dưới:** 2 lớp");
        assert_eq!(list_item("12. done").unwrap().0, "12.");
        assert!(list_item("-5 degrees").is_none());
    }

    #[test]
    fn emphasis_ignores_snake_case() {
        let s: Vec<char> = "box_cardboard_a01".chars().collect();
        assert!(!is_emphasis_marker(&s, 3));
        let s: Vec<char> = "_note_".chars().collect();
        assert!(is_emphasis_marker(&s, 0) && is_emphasis_marker(&s, 5));
    }
}
