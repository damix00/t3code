//! Draws parsed markdown in the chat's typography: 14px prose on a 23px line, SF Mono for code.

use gpui::{
    AnyElement, FontStyle, FontWeight, Hsla, SharedString, StrikethroughStyle, StyledText, TextRun,
    UnderlineStyle, div, font, prelude::*, px,
};

use crate::markdown::{Block, Inline};
use crate::ui::theme;

pub const LINE_HEIGHT: f32 = 22.75;

fn runs(inline: &Inline, color: Hsla, weight: FontWeight) -> Vec<TextRun> {
    let base = |len: usize| TextRun {
        len,
        font: gpui::Font {
            weight,
            ..font(theme::SANS)
        },
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let mut runs = Vec::new();
    let mut at = 0;
    for (range, style) in &inline.spans {
        if range.start > at {
            runs.push(base(range.start - at));
        }
        let mut run = base(range.len());
        if style.code {
            run.font = font(theme::mono());
            run.background_color = Some(theme::code_background());
        }
        if style.bold {
            run.font.weight = FontWeight::SEMIBOLD;
        }
        if style.italic {
            run.font.style = FontStyle::Italic;
        }
        if style.link {
            run.color = theme::info();
            run.underline = Some(UnderlineStyle {
                thickness: px(1.),
                color: Some(theme::info().opacity(0.4)),
                wavy: false,
            });
        }
        if style.strike {
            run.strikethrough = Some(StrikethroughStyle {
                thickness: px(1.),
                color: Some(color),
            });
        }
        runs.push(run);
        at = range.end;
    }
    if at < inline.text.len() {
        runs.push(base(inline.text.len() - at));
    }
    runs
}

fn text(inline: &Inline, color: Hsla, weight: FontWeight) -> StyledText {
    StyledText::new(SharedString::from(inline.text.clone())).with_runs(runs(inline, color, weight))
}

fn paragraph(inline: &Inline) -> gpui::Div {
    div()
        .w_full()
        .child(text(inline, theme::prose(), FontWeight::NORMAL))
}

fn code_block(language: Option<&str>, code: &str, id: SharedString) -> AnyElement {
    div()
        .w_full()
        .flex()
        .flex_col()
        .rounded_lg()
        .border_1()
        .border_color(theme::border())
        .bg(theme::card())
        .overflow_hidden()
        .children(language.map(|language| {
            div()
                .px_3()
                .pt_2()
                .text_size(px(12.))
                .text_color(theme::text_muted())
                .child(language.to_owned())
        }))
        .child(
            div()
                .id(id)
                .overflow_x_scroll()
                .px_3()
                .py_2()
                .font_family(theme::mono())
                .text_size(px(12.5))
                .line_height(px(19.))
                .text_color(theme::prose())
                .whitespace_nowrap()
                .child(code.to_owned()),
        )
        .into_any_element()
}

fn blocks(blocks: &[Block], key: &str) -> Vec<AnyElement> {
    blocks
        .iter()
        .enumerate()
        .map(|(index, block)| render_block(block, &format!("{key}.{index}")))
        .collect()
}

fn render_block(block: &Block, key: &str) -> AnyElement {
    match block {
        Block::Paragraph(inline) => paragraph(inline).into_any_element(),
        Block::Heading {
            level,
            text: inline,
        } => {
            let size = match level {
                1 => 20.,
                2 => 17.,
                _ => 15.,
            };
            div()
                .w_full()
                .pt_1()
                .text_size(px(size))
                .line_height(px(size * 1.4))
                .child(text(inline, theme::text(), FontWeight::SEMIBOLD))
                .into_any_element()
        }
        Block::Code { language, text } => {
            code_block(language.as_deref(), text, format!("code{key}").into())
        }
        Block::List { start, items } => div()
            .w_full()
            .flex()
            .flex_col()
            .gap_1()
            .children(items.iter().enumerate().map(|(index, item)| {
                let marker = match start {
                    Some(first) => format!("{}.", first + index as u64),
                    None => "•".to_owned(),
                };
                div()
                    .flex()
                    .w_full()
                    .child(
                        div()
                            .flex_none()
                            .w(px(22.))
                            .text_color(theme::text_muted())
                            .child(marker),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .children(blocks(item, &format!("{key}.{index}"))),
                    )
            }))
            .into_any_element(),
        Block::Quote(inner) => div()
            .w_full()
            .pl_3()
            .border_l_2()
            .border_color(theme::input())
            .flex()
            .flex_col()
            .gap_2()
            .text_color(theme::text_muted())
            .children(blocks(inner, key))
            .into_any_element(),
        Block::Table { header, rows } => {
            let row = |cells: &[Inline], weight: FontWeight, bordered: bool| {
                div()
                    .flex()
                    .w_full()
                    .when(bordered, |row| {
                        row.border_t_1().border_color(theme::border())
                    })
                    .children(cells.iter().map(|cell| {
                        div().flex_1().min_w_0().px_3().py_1p5().child(text(
                            cell,
                            theme::prose(),
                            weight,
                        ))
                    }))
            };
            div()
                .w_full()
                .flex()
                .flex_col()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .overflow_hidden()
                .child(row(header, FontWeight::SEMIBOLD, false))
                .children(
                    rows.iter()
                        .map(|cells| row(cells, FontWeight::NORMAL, true)),
                )
                .into_any_element()
        }
        Block::Rule => div()
            .w_full()
            .h(px(1.))
            .my_1()
            .bg(theme::border())
            .into_any_element(),
    }
}

/// An assistant message.
pub fn render(parsed: &[Block], key: &str) -> gpui::Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_3()
        .text_size(px(14.))
        .line_height(px(LINE_HEIGHT))
        .text_color(theme::prose())
        .children(blocks(parsed, key))
}
