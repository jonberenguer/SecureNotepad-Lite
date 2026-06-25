//! Minimal Markdown renderer for the live-preview pane.
//!
//! The editor content is always treated as Markdown. This is intentionally a
//! lightweight, dependency-free renderer covering the common constructs
//! (headings, emphasis, inline code, fenced code blocks, lists, blockquotes,
//! horizontal rules, and links) rather than a full CommonMark implementation.

use eframe::egui::{self, Color32, RichText};

// ─── Inline parsing ─────────────────────────────────────────────────────────

struct Seg {
    text:   String,
    bold:   bool,
    italic: bool,
    code:   bool,
    link:   Option<String>,
}

fn push_text(buf: &mut String, segs: &mut Vec<Seg>, bold: bool, italic: bool) {
    if !buf.is_empty() {
        segs.push(Seg { text: std::mem::take(buf), bold, italic, code: false, link: None });
    }
}

/// Split a single line of text into styled segments.
fn parse_inline(s: &str) -> Vec<Seg> {
    let chars: Vec<char> = s.chars().collect();
    let mut segs: Vec<Seg> = Vec::new();
    let mut buf = String::new();
    let mut bold = false;
    let mut italic = false;
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];

        // Inline code span: `code`
        if c == '`' {
            if let Some(close) = (i + 1..chars.len()).find(|&j| chars[j] == '`') {
                push_text(&mut buf, &mut segs, bold, italic);
                let code: String = chars[i + 1..close].iter().collect();
                segs.push(Seg { text: code, bold: false, italic: false, code: true, link: None });
                i = close + 1;
                continue;
            }
        }

        // Bold: ** or __
        if (c == '*' || c == '_') && i + 1 < chars.len() && chars[i + 1] == c {
            push_text(&mut buf, &mut segs, bold, italic);
            bold = !bold;
            i += 2;
            continue;
        }

        // Italic: * or _
        if c == '*' || c == '_' {
            push_text(&mut buf, &mut segs, bold, italic);
            italic = !italic;
            i += 1;
            continue;
        }

        // Link: [text](url)
        if c == '[' {
            if let Some(cb) = (i + 1..chars.len()).find(|&j| chars[j] == ']') {
                if cb + 1 < chars.len() && chars[cb + 1] == '(' {
                    if let Some(cp) = (cb + 2..chars.len()).find(|&j| chars[j] == ')') {
                        push_text(&mut buf, &mut segs, bold, italic);
                        let text: String = chars[i + 1..cb].iter().collect();
                        let url:  String = chars[cb + 2..cp].iter().collect();
                        segs.push(Seg { text, bold, italic, code: false, link: Some(url) });
                        i = cp + 1;
                        continue;
                    }
                }
            }
        }

        buf.push(c);
        i += 1;
    }
    push_text(&mut buf, &mut segs, bold, italic);
    segs
}

/// Add the styled segments to the current (already laid-out) ui.
fn render_segments(ui: &mut egui::Ui, segs: &[Seg], size: f32, dark: bool, force_bold: bool) {
    let link_color = Color32::from_rgb(124, 106, 247);
    let code_bg    = if dark { Color32::from_rgb(40, 40, 50) } else { Color32::from_rgb(224, 224, 219) };
    let code_fg    = if dark { Color32::from_rgb(224, 184, 128) } else { Color32::from_rgb(168, 84, 40) };

    for seg in segs {
        if let Some(url) = &seg.link {
            ui.hyperlink_to(RichText::new(&seg.text).size(size).color(link_color), url);
        } else if seg.code {
            ui.label(
                RichText::new(&seg.text)
                    .monospace()
                    .size(size)
                    .background_color(code_bg)
                    .color(code_fg),
            );
        } else {
            let mut rt = RichText::new(&seg.text).size(size);
            if seg.bold || force_bold { rt = rt.strong(); }
            if seg.italic { rt = rt.italics(); }
            ui.label(rt);
        }
    }
}

/// Render a single line's inline content inside a wrapping row.
fn render_inline(ui: &mut egui::Ui, line: &str, size: f32, dark: bool, force_bold: bool) {
    let segs = parse_inline(line);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        render_segments(ui, &segs, size, dark, force_bold);
    });
}

// ─── Block helpers ──────────────────────────────────────────────────────────

fn heading_level(s: &str) -> Option<usize> {
    let hashes = s.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) && s[hashes..].starts_with(' ') {
        Some(hashes)
    } else {
        None
    }
}

fn strip_bullet(s: &str) -> Option<&str> {
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = s.strip_prefix(marker) {
            return Some(rest);
        }
    }
    None
}

fn ordered_item(s: &str) -> Option<(&str, &str)> {
    let digits = s.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let rest = &s[digits..];
    if let Some(after) = rest.strip_prefix(". ") {
        Some((&s[..digits], after))
    } else {
        None
    }
}

fn leading_spaces(s: &str) -> usize {
    s.chars().take_while(|&c| c == ' ' || c == '\t').count()
}

// ─── Public entry point ─────────────────────────────────────────────────────

/// Render `text` as Markdown into `ui`. `base` is the base font size (pixels).
pub fn render(ui: &mut egui::Ui, text: &str, base: f32, dark: bool) {
    let lines: Vec<&str> = text.split('\n').collect();
    let code_block_bg = if dark { Color32::from_rgb(30, 30, 38) } else { Color32::from_rgb(228, 228, 222) };
    let quote_color   = if dark { Color32::from_rgb(150, 150, 170) } else { Color32::from_rgb(90, 90, 110) };
    let quote_bar     = Color32::from_rgb(124, 106, 247);

    let mut i = 0;
    while i < lines.len() {
        let line    = lines[i];
        let trimmed = line.trim_start();

        // Fenced code block: ``` ... ```
        if trimmed.starts_with("```") {
            i += 1;
            let mut code_lines: Vec<&str> = Vec::new();
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                code_lines.push(lines[i]);
                i += 1;
            }
            if i < lines.len() {
                i += 1; // consume closing fence
            }
            egui::Frame::none()
                .fill(code_block_bg)
                .inner_margin(egui::Margin::same(8.0))
                .rounding(4.0)
                .show(ui, |ui| {
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        if code_lines.is_empty() {
                            ui.label(RichText::new(" ").monospace().size(base));
                        }
                        for cl in &code_lines {
                            ui.label(RichText::new(*cl).monospace().size(base));
                        }
                    });
                });
            ui.add_space(4.0);
            continue;
        }

        // Blank line → paragraph spacing
        if trimmed.is_empty() {
            ui.add_space(base * 0.5);
            i += 1;
            continue;
        }

        // Horizontal rule
        if trimmed == "---" || trimmed == "***" || trimmed == "___" {
            ui.add_space(2.0);
            ui.separator();
            ui.add_space(2.0);
            i += 1;
            continue;
        }

        // Heading
        if let Some(level) = heading_level(trimmed) {
            let content = trimmed[level..].trim_start();
            let scale = match level {
                1 => 1.9,
                2 => 1.55,
                3 => 1.3,
                4 => 1.15,
                5 => 1.05,
                _ => 1.0,
            };
            ui.add_space(base * 0.3);
            render_inline(ui, content, base * scale, dark, true);
            ui.add_space(base * 0.15);
            i += 1;
            continue;
        }

        // Blockquote
        if let Some(rest) = trimmed.strip_prefix('>') {
            let q = rest.strip_prefix(' ').unwrap_or(rest);
            ui.horizontal(|ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::Vec2::new(3.0, base * 1.3), egui::Sense::hover());
                ui.painter().rect_filled(rect, 0.0, quote_bar);
                ui.add_space(6.0);
                ui.scope(|ui| {
                    ui.visuals_mut().override_text_color = Some(quote_color);
                    render_inline(ui, q, base, dark, false);
                });
            });
            i += 1;
            continue;
        }

        // Unordered list item
        if let Some(rest) = strip_bullet(trimmed) {
            let indent = leading_spaces(line);
            let segs = parse_inline(rest);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.add_space(12.0 + indent as f32 * 8.0);
                ui.label(RichText::new("•  ").size(base));
                render_segments(ui, &segs, base, dark, false);
            });
            i += 1;
            continue;
        }

        // Ordered list item
        if let Some((num, rest)) = ordered_item(trimmed) {
            let indent = leading_spaces(line);
            let segs = parse_inline(rest);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.add_space(12.0 + indent as f32 * 8.0);
                ui.label(RichText::new(format!("{}.  ", num)).size(base));
                render_segments(ui, &segs, base, dark, false);
            });
            i += 1;
            continue;
        }

        // Plain paragraph line
        render_inline(ui, line, base, dark, false);
        i += 1;
    }
}
