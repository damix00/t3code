//! Markdown for assistant messages, parsed into blocks of styled text the views can draw.

use std::ops::Range;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// How a run of inline text is styled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strike: bool,
    pub link: bool,
}

/// A paragraph's text and its styled ranges (byte offsets, never overlapping, in order).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inline {
    pub text: String,
    pub spans: Vec<(Range<usize>, Style)>,
}

impl Inline {
    fn push(&mut self, text: &str, style: Style) {
        if text.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(text);
        if style != Style::default() {
            match self.spans.last_mut() {
                Some((range, last)) if *last == style && range.end == start => {
                    range.end = self.text.len()
                }
                _ => self.spans.push((start..self.text.len(), style)),
            }
        }
    }

    fn trimmed(mut self) -> Self {
        let trimmed_end = self.text.trim_end().len();
        self.text.truncate(trimmed_end);
        self.spans.retain_mut(|(range, _)| {
            range.end = range.end.min(trimmed_end);
            range.start < range.end
        });
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Inline),
    Heading {
        level: u8,
        text: Inline,
    },
    Code {
        language: Option<String>,
        text: String,
    },
    /// `start` is the first number of an ordered list.
    List {
        start: Option<u64>,
        items: Vec<Vec<Block>>,
    },
    Quote(Vec<Block>),
    Table {
        header: Vec<Inline>,
        rows: Vec<Vec<Inline>>,
    },
    Rule,
}

enum Frame {
    Quote(Vec<Block>),
    List {
        start: Option<u64>,
        items: Vec<Vec<Block>>,
    },
    Item(Vec<Block>),
}

impl Frame {
    fn blocks(&mut self) -> &mut Vec<Block> {
        match self {
            Self::Quote(blocks) | Self::Item(blocks) => blocks,
            Self::List { items, .. } => {
                if items.is_empty() {
                    items.push(Vec::new());
                }
                let last = items.len() - 1;
                &mut items[last]
            }
        }
    }
}

struct Builder {
    root: Vec<Block>,
    frames: Vec<Frame>,
    inline: Option<Inline>,
    style: Style,
    code: Option<(Option<String>, String)>,
    table: Option<(Vec<Inline>, Vec<Vec<Inline>>, bool)>,
    heading: Option<u8>,
}

impl Builder {
    fn blocks(&mut self) -> &mut Vec<Block> {
        match self.frames.last_mut() {
            Some(frame) => frame.blocks(),
            None => &mut self.root,
        }
    }

    fn inline(&mut self) -> &mut Inline {
        self.inline.get_or_insert_with(Inline::default)
    }

    /// Ends loose text (a tight list item's text has no paragraph tag) as a paragraph.
    fn flush_inline(&mut self) {
        if self.table.is_some() {
            return;
        }
        if let Some(inline) = self
            .inline
            .take()
            .map(Inline::trimmed)
            .filter(|inline| !inline.text.is_empty())
        {
            let block = match self.heading {
                Some(level) => Block::Heading {
                    level,
                    text: inline,
                },
                None => Block::Paragraph(inline),
            };
            self.blocks().push(block);
        }
    }

    fn text(&mut self, text: &str) {
        if let Some((_, code)) = &mut self.code {
            code.push_str(text);
        } else {
            let style = self.style;
            self.inline().push(text, style);
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => self.flush_inline(),
            Tag::Heading { level, .. } => {
                self.flush_inline();
                self.heading = Some(match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                });
            }
            Tag::BlockQuote(_) => {
                self.flush_inline();
                self.frames.push(Frame::Quote(Vec::new()));
            }
            Tag::CodeBlock(kind) => {
                self.flush_inline();
                let language = match kind {
                    CodeBlockKind::Fenced(info) => info
                        .split_whitespace()
                        .next()
                        .map(str::to_owned)
                        .filter(|language| !language.is_empty()),
                    CodeBlockKind::Indented => None,
                };
                self.code = Some((language, String::new()));
            }
            Tag::List(start) => {
                self.flush_inline();
                self.frames.push(Frame::List {
                    start,
                    items: Vec::new(),
                });
            }
            Tag::Item => {
                self.flush_inline();
                self.frames.push(Frame::Item(Vec::new()));
            }
            Tag::Table(_) => {
                self.flush_inline();
                self.table = Some((Vec::new(), Vec::new(), true));
            }
            Tag::TableHead => {}
            Tag::TableRow => {
                if let Some((_, rows, _)) = &mut self.table {
                    rows.push(Vec::new());
                }
            }
            Tag::TableCell => self.inline = Some(Inline::default()),
            Tag::Emphasis => self.style.italic = true,
            Tag::Strong => self.style.bold = true,
            Tag::Strikethrough => self.style.strike = true,
            Tag::Link { .. } => self.style.link = true,
            Tag::Image { .. } => {}
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.flush_inline(),
            TagEnd::Heading(_) => {
                self.flush_inline();
                self.heading = None;
            }
            TagEnd::BlockQuote(_) => {
                self.flush_inline();
                if let Some(Frame::Quote(blocks)) = self.frames.pop() {
                    self.blocks().push(Block::Quote(blocks));
                }
            }
            TagEnd::CodeBlock => {
                if let Some((language, mut text)) = self.code.take() {
                    if text.ends_with('\n') {
                        text.pop();
                    }
                    self.blocks().push(Block::Code { language, text });
                }
            }
            TagEnd::Item => {
                self.flush_inline();
                if let Some(Frame::Item(blocks)) = self.frames.pop()
                    && let Some(Frame::List { items, .. }) = self.frames.last_mut()
                {
                    items.push(blocks);
                }
            }
            TagEnd::List(_) => {
                self.flush_inline();
                if let Some(Frame::List { start, items }) = self.frames.pop() {
                    self.blocks().push(Block::List { start, items });
                }
            }
            TagEnd::TableHead => {
                if let Some((_, _, in_head)) = &mut self.table {
                    *in_head = false;
                }
            }
            TagEnd::TableCell => {
                let cell = self.inline.take().map(Inline::trimmed).unwrap_or_default();
                if let Some((header, rows, in_head)) = &mut self.table {
                    if *in_head {
                        header.push(cell);
                    } else if let Some(row) = rows.last_mut() {
                        row.push(cell);
                    }
                }
            }
            TagEnd::Table => {
                if let Some((header, rows, _)) = self.table.take() {
                    self.blocks().push(Block::Table { header, rows });
                }
            }
            TagEnd::Emphasis => self.style.italic = false,
            TagEnd::Strong => self.style.bold = false,
            TagEnd::Strikethrough => self.style.strike = false,
            TagEnd::Link => self.style.link = false,
            _ => {}
        }
    }
}

/// Parses CommonMark with tables and strikethrough.
pub fn parse(markdown: &str) -> Vec<Block> {
    let mut builder = Builder {
        root: Vec::new(),
        frames: Vec::new(),
        inline: None,
        style: Style::default(),
        code: None,
        table: None,
        heading: None,
    };
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    for event in Parser::new_ext(markdown, options) {
        match event {
            Event::Start(tag) => builder.start(tag),
            Event::End(tag) => builder.end(tag),
            Event::Text(text) => builder.text(&text),
            Event::Code(code) => {
                let style = Style {
                    code: true,
                    ..builder.style
                };
                builder.inline().push(&code, style);
            }
            Event::SoftBreak => builder.text(" "),
            Event::HardBreak => builder.text("\n"),
            Event::Rule => {
                builder.flush_inline();
                builder.blocks().push(Block::Rule);
            }
            Event::Html(html) | Event::InlineHtml(html) => builder.text(&html),
            _ => {}
        }
    }
    builder.flush_inline();
    // Close anything an unterminated stream left open.
    while !builder.frames.is_empty() {
        builder.end(match builder.frames.last() {
            Some(Frame::Quote(_)) => TagEnd::BlockQuote(None),
            Some(Frame::Item(_)) => TagEnd::Item,
            _ => TagEnd::List(false),
        });
    }
    builder.root
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn plain(text: &str) -> Inline {
        Inline {
            text: text.into(),
            spans: Vec::new(),
        }
    }

    const BOLD: Style = Style {
        bold: true,
        italic: false,
        code: false,
        strike: false,
        link: false,
    };
    const CODE: Style = Style {
        bold: false,
        italic: false,
        code: true,
        strike: false,
        link: false,
    };

    #[test]
    fn paragraphs_carry_inline_styles() {
        let blocks = parse("Your macOS is in **Dark** mode, see `theme.rs`.\nNext line.");
        assert_eq!(
            blocks,
            vec![Block::Paragraph(Inline {
                text: "Your macOS is in Dark mode, see theme.rs. Next line.".into(),
                spans: vec![(17..21, BOLD), (32..40, CODE)],
            })]
        );
    }

    #[test]
    fn headings_code_and_rules() {
        let blocks = parse("## Status\n\n```rust\nfn main() {}\n```\n\n---\n");
        assert_eq!(
            blocks,
            vec![
                Block::Heading {
                    level: 2,
                    text: plain("Status")
                },
                Block::Code {
                    language: Some("rust".into()),
                    text: "fn main() {}".into()
                },
                Block::Rule,
            ]
        );
    }

    #[test]
    fn lists_nest() {
        let blocks = parse("1. one\n2. two\n   - inner\n");
        let Block::List { start, items } = &blocks[0] else {
            panic!("{blocks:?}");
        };
        assert_eq!(*start, Some(1));
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], vec![Block::Paragraph(plain("one"))]);
        assert_eq!(items[1][0], Block::Paragraph(plain("two")));
        assert_eq!(
            items[1][1],
            Block::List {
                start: None,
                items: vec![vec![Block::Paragraph(plain("inner"))]]
            }
        );
    }

    #[test]
    fn quotes_and_tables() {
        let blocks = parse("> careful\n\n| a | b |\n| - | - |\n| 1 | **2** |\n");
        assert_eq!(
            blocks[0],
            Block::Quote(vec![Block::Paragraph(plain("careful"))])
        );
        let Block::Table { header, rows } = &blocks[1] else {
            panic!("{blocks:?}");
        };
        assert_eq!(header, &vec![plain("a"), plain("b")]);
        assert_eq!(rows[0][0], plain("1"));
        assert_eq!(rows[0][1].spans, vec![(0..1, BOLD)]);
    }

    #[test]
    fn streaming_text_parses_partially() {
        // An unterminated fence mid-stream still shows as code.
        let blocks = parse("Here:\n\n```sh\nls -la");
        assert_eq!(
            blocks[1],
            Block::Code {
                language: Some("sh".into()),
                text: "ls -la".into()
            }
        );
        assert_eq!(parse(""), Vec::new());
        assert_eq!(
            parse("- a\n- b")[0],
            Block::List {
                start: None,
                items: vec![
                    vec![Block::Paragraph(plain("a"))],
                    vec![Block::Paragraph(plain("b"))]
                ],
            }
        );
    }

    #[test]
    fn adjacent_runs_merge_and_links_are_marked() {
        let blocks = parse("see [the **docs**](https://x) now");
        let Block::Paragraph(inline) = &blocks[0] else {
            panic!()
        };
        assert_eq!(inline.text, "see the docs now");
        assert_eq!(inline.spans.len(), 2);
        assert!(inline.spans.iter().all(|(_, style)| style.link));
    }
}
