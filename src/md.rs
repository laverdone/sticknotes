// Markdown renderer for GtkTextView.
//
// The note body stays plain Markdown text (that is what the user edits and
// what is persisted).  The preview is rendered into a *separate* TextBuffer
// using TextTags, and images are inserted as child anchors holding a
// GtkPicture: this way Markdown images (including the ones pasted from the
// clipboard and stored in SQLite as `sticknotes://<asset-id>`) are really
// displayed, which is not possible with Pango markup alone.

use gtk::glib::ToValue;
use gtk::prelude::*;
use gtk::{gdk, pango, TextBuffer, TextChildAnchor, TextIter, TextTag};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag};

// ---------------------------------------------------------------------------
// Tags
// ---------------------------------------------------------------------------

struct Tags {
    heading: [TextTag; 6],
    bold: TextTag,
    italic: TextTag,
    strike: TextTag,
    code: TextTag,
    code_block: TextTag,
    quote: TextTag,
    link: TextTag,
    dim: TextTag,
    rule: TextTag,
}

fn ensure_tag(buffer: &TextBuffer, name: &str, properties: &[(&str, &dyn ToValue)]) -> TextTag {
    if let Some(tag) = buffer.tag_table().lookup(name) {
        return tag;
    }
    buffer
        .create_tag(Some(name), properties)
        .unwrap_or_else(|| TextTag::new(Some(name)))
}

impl Tags {
    fn new(buffer: &TextBuffer) -> Self {
        // GtkTextTag::weight is a plain gint, not a PangoWeight enum.
        let bold: i32 = 700;
        let heading = std::array::from_fn(|i| {
            let scale: f64 = [2.0, 1.65, 1.4, 1.25, 1.12, 1.0][i];
            let above: i32 = if i == 0 { 14 } else { 10 };
            ensure_tag(
                buffer,
                &format!("md-h{}", i + 1),
                &[
                    ("scale", &scale),
                    ("weight", &bold),
                    ("pixels-above-lines", &above),
                ],
            )
        });

        Self {
            heading,
            bold: ensure_tag(buffer, "md-bold", &[("weight", &bold)]),
            italic: ensure_tag(buffer, "md-italic", &[("style", &pango::Style::Italic)]),
            strike: ensure_tag(buffer, "md-strike", &[("strikethrough", &true)]),
            code: ensure_tag(
                buffer,
                "md-code",
                &[
                    ("family", &"monospace"),
                    ("background", &"rgba(0,0,0,0.08)"),
                ],
            ),
            code_block: ensure_tag(
                buffer,
                "md-code-block",
                &[
                    ("family", &"monospace"),
                    ("left-margin", &12i32),
                    ("background", &"rgba(0,0,0,0.05)"),
                ],
            ),
            quote: ensure_tag(
                buffer,
                "md-quote",
                &[
                    ("style", &pango::Style::Italic),
                    ("foreground", &"#5e5c64"),
                    ("left-margin", &14i32),
                ],
            ),
            link: ensure_tag(
                buffer,
                "md-link",
                &[
                    ("foreground", &"#1c71d8"),
                    ("underline", &pango::Underline::Single),
                ],
            ),
            dim: ensure_tag(buffer, "md-dim", &[("foreground", &"#7a7a7a")]),
            rule: ensure_tag(
                buffer,
                "md-rule",
                &[
                    ("foreground", &"#b0b0b0"),
                    ("justification", &gtk::Justification::Center),
                ],
            ),
        }
    }
}

fn put(buffer: &TextBuffer, iter: &mut TextIter, tags: &[TextTag], text: &str) {
    let refs: Vec<&TextTag> = tags.iter().collect();
    buffer.insert_with_tags(iter, text, &refs);
}

fn heading_index(level: HeadingLevel) -> usize {
    match level {
        HeadingLevel::H1 => 0,
        HeadingLevel::H2 => 1,
        HeadingLevel::H3 => 2,
        HeadingLevel::H4 => 3,
        HeadingLevel::H5 => 4,
        HeadingLevel::H6 => 5,
    }
}

// ---------------------------------------------------------------------------
// Renderer
// ---------------------------------------------------------------------------

/// Render `markdown` into `buffer` (which is normally the buffer of a
/// read-only preview TextView).
///
/// * `resolve_image` maps an image destination (`sticknotes://<id>`, or an
///   absolute file path) to a texture.
/// * `add_image` is called for every resolved image together with the child
///   anchor created in the buffer: the caller is responsible for putting a
///   widget there (see `note::render_preview`).
pub fn render(
    buffer: &TextBuffer,
    markdown: &str,
    resolve_image: &dyn Fn(&str) -> Option<gdk::Texture>,
    add_image: &mut dyn FnMut(&TextChildAnchor, &gdk::Texture, &str),
) {
    buffer.set_text("");

    let tags = Tags::new(buffer);
    let mut iter = buffer.end_iter();
    let mut stack: Vec<TextTag> = Vec::new();
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut in_image = false;
    let mut image_alt = String::new();
    let mut row_first = true;

    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;

    for event in Parser::new_ext(markdown, options) {
        match event {
            Event::Start(Tag::Paragraph) => {}
            Event::End(Tag::Paragraph) => put(buffer, &mut iter, &stack, "\n"),

            Event::Start(Tag::Heading(level, _, _)) => {
                stack.push(tags.heading[heading_index(level)].clone());
            }
            Event::End(Tag::Heading(_, _, _)) => {
                stack.pop();
                put(buffer, &mut iter, &stack, "\n");
            }

            Event::Start(Tag::BlockQuote) => stack.push(tags.quote.clone()),
            Event::End(Tag::BlockQuote) => {
                stack.pop();
                put(buffer, &mut iter, &stack, "\n");
            }

            Event::Start(Tag::CodeBlock(_)) => {
                stack.push(tags.code_block.clone());
                put(buffer, &mut iter, &stack, "\n");
            }
            Event::End(Tag::CodeBlock(_)) => {
                stack.pop();
                put(buffer, &mut iter, &stack, "\n");
            }

            Event::Start(Tag::List(start)) => lists.push(start),
            Event::End(Tag::List(_)) => {
                lists.pop();
            }
            Event::Start(Tag::Item) => {
                let indent = "    ".repeat(lists.len().saturating_sub(1));
                let marker = match lists.last_mut() {
                    Some(Some(next)) => {
                        let marker = format!("{next}.");
                        *next += 1;
                        marker
                    }
                    _ => "•".to_string(),
                };
                put(buffer, &mut iter, &stack, &format!("{indent}{marker} "));
            }
            Event::End(Tag::Item) => put(buffer, &mut iter, &stack, "\n"),

            Event::Start(Tag::Emphasis) => stack.push(tags.italic.clone()),
            Event::End(Tag::Emphasis) => {
                stack.pop();
            }
            Event::Start(Tag::Strong) => stack.push(tags.bold.clone()),
            Event::End(Tag::Strong) => {
                stack.pop();
            }
            Event::Start(Tag::Strikethrough) => stack.push(tags.strike.clone()),
            Event::End(Tag::Strikethrough) => {
                stack.pop();
            }

            Event::Start(Tag::Link(_, _, _)) => stack.push(tags.link.clone()),
            Event::End(Tag::Link(_, _, _)) => {
                stack.pop();
            }

            Event::Start(Tag::Image(_, _, _)) => {
                in_image = true;
                image_alt.clear();
            }
            Event::End(Tag::Image(_, dest, _)) => {
                in_image = false;
                let source = dest.to_string();
                match resolve_image(&source) {
                    Some(texture) => {
                        let anchor = buffer.create_child_anchor(&mut iter);
                        add_image(&anchor, &texture, &image_alt);
                        put(buffer, &mut iter, &stack, "\n");
                    }
                    None => {
                        let label = if image_alt.trim().is_empty() {
                            "immagine".to_string()
                        } else {
                            image_alt.clone()
                        };
                        put(
                            buffer,
                            &mut iter,
                            std::slice::from_ref(&tags.dim),
                            &format!("[🖼 {label}]\n"),
                        );
                    }
                }
            }

            Event::Text(text) => {
                if in_image {
                    image_alt.push_str(&text);
                } else {
                    put(buffer, &mut iter, &stack, &text);
                }
            }
            Event::Code(code) => put(buffer, &mut iter, std::slice::from_ref(&tags.code), &code),
            Event::Html(html) => put(buffer, &mut iter, &stack, &html),
            Event::FootnoteReference(name) => put(buffer, &mut iter, &stack, &format!("[{name}]")),
            Event::SoftBreak => put(buffer, &mut iter, &stack, " "),
            Event::HardBreak => put(buffer, &mut iter, &stack, "\n"),
            Event::Rule => put(
                buffer,
                &mut iter,
                std::slice::from_ref(&tags.rule),
                "────────────────────────\n",
            ),
            Event::TaskListMarker(done) => {
                put(buffer, &mut iter, &stack, if done { "☑ " } else { "☐ " })
            }

            Event::Start(Tag::Table(_)) => put(buffer, &mut iter, &stack, "\n"),
            Event::End(Tag::Table(_)) => put(buffer, &mut iter, &stack, "\n"),
            Event::Start(Tag::TableHead) | Event::Start(Tag::TableRow) => row_first = true,
            Event::End(Tag::TableHead) | Event::End(Tag::TableRow) => {
                put(buffer, &mut iter, &stack, "\n")
            }
            Event::Start(Tag::TableCell) => {
                if !row_first {
                    put(buffer, &mut iter, &stack, "  │  ");
                }
                row_first = false;
            }
            Event::End(Tag::TableCell) => {}

            Event::Start(Tag::FootnoteDefinition(_)) => put(buffer, &mut iter, &stack, "\n"),
            Event::End(Tag::FootnoteDefinition(_)) => put(buffer, &mut iter, &stack, "\n"),
        }
    }
}

// ---------------------------------------------------------------------------
// Title derivation
// ---------------------------------------------------------------------------

/// Derive the window/list title from the Markdown body: the first `# heading`
/// wins, otherwise the first non-empty, non-image line.
pub fn derived_title(markdown: &str) -> String {
    for line in markdown.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("# ") {
            let title = rest.trim();
            if !title.is_empty() {
                return truncate(title, 60);
            }
        }
    }
    for line in markdown.lines() {
        let line = line.trim();
        if line.is_empty()
            || line.starts_with('#')
            || line.starts_with("![")
            || line.starts_with("---")
            || line.starts_with("```")
        {
            continue;
        }
        return truncate(line, 60);
    }
    "Nota vuota".to_string()
}

fn truncate(text: &str, max_chars: usize) -> String {
    let mut out: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_from_heading() {
        assert_eq!(derived_title("# Ciao **mondo**\n\ncorpo"), "Ciao **mondo**");
    }

    #[test]
    fn title_from_first_line() {
        assert_eq!(derived_title("\n\nsolo testo\n"), "solo testo");
    }

    #[test]
    fn empty_note() {
        assert_eq!(derived_title(""), "Nota vuota");
    }
}
