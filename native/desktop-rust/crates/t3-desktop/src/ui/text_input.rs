//! A multi-line, wrapping text input on top of [`EditBuffer`], with IME support through
//! GPUI's [`EntityInputHandler`]. Based on GPUI's `input` example.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyBinding,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    SharedString, Style, TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window, WrappedLine,
    actions, div, fill, point, prelude::*, px, relative, size,
};

use crate::editor::EditBuffer;
use crate::ui::theme;

actions!(
    text_input,
    [
        Backspace,
        Delete,
        DeleteWordLeft,
        DeleteToLineStart,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        LineStart,
        LineEnd,
        SelectLineStart,
        SelectLineEnd,
        DocumentStart,
        DocumentEnd,
        SelectAll,
        Newline,
        Submit,
        Paste,
        Cut,
        Copy,
    ]
);

const CONTEXT: &str = "TextInput";

/// Key bindings for every text input. Platform conventions: `cmd` on macOS, `ctrl` elsewhere.
pub fn bindings() -> Vec<KeyBinding> {
    let context = Some(CONTEXT);
    let mut bindings = vec![
        KeyBinding::new("backspace", Backspace, context),
        KeyBinding::new("shift-backspace", Backspace, context),
        KeyBinding::new("delete", Delete, context),
        KeyBinding::new("left", Left, context),
        KeyBinding::new("right", Right, context),
        KeyBinding::new("up", Up, context),
        KeyBinding::new("down", Down, context),
        KeyBinding::new("shift-left", SelectLeft, context),
        KeyBinding::new("shift-right", SelectRight, context),
        KeyBinding::new("shift-up", SelectUp, context),
        KeyBinding::new("shift-down", SelectDown, context),
        KeyBinding::new("home", LineStart, context),
        KeyBinding::new("end", LineEnd, context),
        KeyBinding::new("shift-home", SelectLineStart, context),
        KeyBinding::new("shift-end", SelectLineEnd, context),
        KeyBinding::new("enter", Submit, context),
        KeyBinding::new("shift-enter", Newline, context),
        KeyBinding::new("alt-enter", Newline, context),
    ];
    if cfg!(target_os = "macos") {
        bindings.extend([
            KeyBinding::new("alt-backspace", DeleteWordLeft, context),
            KeyBinding::new("cmd-backspace", DeleteToLineStart, context),
            KeyBinding::new("alt-left", WordLeft, context),
            KeyBinding::new("alt-right", WordRight, context),
            KeyBinding::new("alt-shift-left", SelectWordLeft, context),
            KeyBinding::new("alt-shift-right", SelectWordRight, context),
            KeyBinding::new("cmd-left", LineStart, context),
            KeyBinding::new("cmd-right", LineEnd, context),
            KeyBinding::new("cmd-shift-left", SelectLineStart, context),
            KeyBinding::new("cmd-shift-right", SelectLineEnd, context),
            KeyBinding::new("cmd-up", DocumentStart, context),
            KeyBinding::new("cmd-down", DocumentEnd, context),
            KeyBinding::new("cmd-a", SelectAll, context),
            KeyBinding::new("cmd-v", Paste, context),
            KeyBinding::new("cmd-c", Copy, context),
            KeyBinding::new("cmd-x", Cut, context),
            KeyBinding::new("cmd-enter", Submit, context),
        ]);
    } else {
        bindings.extend([
            KeyBinding::new("ctrl-backspace", DeleteWordLeft, context),
            KeyBinding::new("ctrl-left", WordLeft, context),
            KeyBinding::new("ctrl-right", WordRight, context),
            KeyBinding::new("ctrl-shift-left", SelectWordLeft, context),
            KeyBinding::new("ctrl-shift-right", SelectWordRight, context),
            KeyBinding::new("ctrl-home", DocumentStart, context),
            KeyBinding::new("ctrl-end", DocumentEnd, context),
            KeyBinding::new("ctrl-a", SelectAll, context),
            KeyBinding::new("ctrl-v", Paste, context),
            KeyBinding::new("ctrl-c", Copy, context),
            KeyBinding::new("ctrl-x", Cut, context),
            KeyBinding::new("ctrl-enter", Submit, context),
        ]);
    }
    bindings
}

/// What a text input tells its owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextInputEvent {
    /// Enter was pressed.
    Submit,
    Changed,
}

/// Where each logical line was painted last frame.
#[derive(Clone)]
struct PaintedLayout {
    bounds: Bounds<Pixels>,
    line_height: Pixels,
    /// Start offset, top, and shaped text of each logical line.
    lines: Vec<(usize, Pixels, WrappedLine)>,
}

impl PaintedLayout {
    fn line_for_offset(&self, offset: usize) -> Option<&(usize, Pixels, WrappedLine)> {
        self.lines
            .iter()
            .rev()
            .find(|(start, _, _)| *start <= offset)
    }

    /// The caret's top-left for `offset`, relative to the input's origin.
    fn position_for(&self, offset: usize) -> Point<Pixels> {
        let Some((start, top, line)) = self.line_for_offset(offset) else {
            return point(px(0.), px(0.));
        };
        let local = offset.saturating_sub(*start).min(line.len());
        let position = line
            .position_for_index(local, self.line_height)
            .unwrap_or_else(|| {
                let rows = line.wrap_boundaries().len();
                point(line.width(), self.line_height * rows as f32)
            });
        point(position.x, *top + position.y)
    }

    /// The offset nearest to a window position.
    fn index_for(&self, position: Point<Pixels>) -> usize {
        let local = position - self.bounds.origin;
        let Some((start, top, line)) = self
            .lines
            .iter()
            .rev()
            .find(|(_, top, _)| local.y >= *top)
            .or_else(|| self.lines.first())
        else {
            return 0;
        };
        let height = line.size(self.line_height).height;
        let y = (local.y - *top)
            .max(px(0.))
            .min(height - px(1.))
            .max(px(0.));
        let index = match line
            .closest_index_for_position(point(local.x.max(px(0.)), y), self.line_height)
        {
            Ok(index) | Err(index) => index,
        };
        start + index.min(line.len())
    }
}

pub struct TextInput {
    focus_handle: FocusHandle,
    buffer: EditBuffer,
    placeholder: SharedString,
    layout: Option<PaintedLayout>,
    is_selecting: bool,
    disabled: bool,
}

impl EventEmitter<TextInputEvent> for TextInput {}

impl std::fmt::Debug for TextInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextInput")
            .field("buffer", &self.buffer)
            .field("disabled", &self.disabled)
            .finish()
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl TextInput {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            buffer: EditBuffer::default(),
            placeholder: placeholder.into(),
            layout: None,
            is_selecting: false,
            disabled: false,
        }
    }

    pub fn text(&self) -> &str {
        self.buffer.text()
    }

    pub fn is_blank(&self) -> bool {
        self.buffer.is_blank()
    }

    pub fn set_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let placeholder = placeholder.into();
        if placeholder != self.placeholder {
            self.placeholder = placeholder;
            cx.notify();
        }
    }

    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.buffer.set_text(text);
        self.changed(cx);
    }

    pub fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        if self.disabled != disabled {
            self.disabled = disabled;
            cx.notify();
        }
    }

    /// Clears the input and returns its text.
    pub fn take(&mut self, cx: &mut Context<Self>) -> String {
        let text = self.buffer.take();
        self.changed(cx);
        text
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(TextInputEvent::Changed);
        cx.notify();
    }

    fn moved(&mut self, cx: &mut Context<Self>) {
        cx.notify();
    }

    fn edit(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut EditBuffer)) {
        if self.disabled {
            return;
        }
        edit(&mut self.buffer);
        self.changed(cx);
    }

    fn vertical_target(&self, rows: f32) -> Option<usize> {
        let layout = self.layout.as_ref()?;
        let caret = layout.position_for(self.buffer.cursor());
        let target =
            layout.bounds.origin + point(caret.x, caret.y + layout.line_height * (rows + 0.5));
        if target.y < layout.bounds.top() {
            return Some(0);
        }
        if target.y > layout.bounds.bottom() {
            return Some(self.buffer.text().len());
        }
        Some(layout.index_for(target))
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        self.edit(cx, EditBuffer::backspace);
    }
    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        self.edit(cx, EditBuffer::delete);
    }
    fn delete_word_left(&mut self, _: &DeleteWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.edit(cx, EditBuffer::delete_word_left);
    }
    fn delete_to_line_start(
        &mut self,
        _: &DeleteToLineStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit(cx, EditBuffer::delete_to_line_start);
    }
    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_left(false);
        self.moved(cx);
    }
    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_right(false);
        self.moved(cx);
    }
    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_left(true);
        self.moved(cx);
    }
    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_right(true);
        self.moved(cx);
    }
    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        let target = self.vertical_target(-1.).unwrap_or(0);
        self.buffer.move_to(target);
        self.moved(cx);
    }
    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        let target = self.vertical_target(1.).unwrap_or(self.buffer.text().len());
        self.buffer.move_to(target);
        self.moved(cx);
    }
    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        let target = self.vertical_target(-1.).unwrap_or(0);
        self.buffer.select_to(target);
        self.moved(cx);
    }
    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        let target = self.vertical_target(1.).unwrap_or(self.buffer.text().len());
        self.buffer.select_to(target);
        self.moved(cx);
    }
    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_word_left(false);
        self.moved(cx);
    }
    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_word_right(false);
        self.moved(cx);
    }
    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_word_left(true);
        self.moved(cx);
    }
    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_word_right(true);
        self.moved(cx);
    }
    fn line_start(&mut self, _: &LineStart, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_line_start(false);
        self.moved(cx);
    }
    fn line_end(&mut self, _: &LineEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_line_end(false);
        self.moved(cx);
    }
    fn select_line_start(&mut self, _: &SelectLineStart, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_line_start(true);
        self.moved(cx);
    }
    fn select_line_end(&mut self, _: &SelectLineEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_line_end(true);
        self.moved(cx);
    }
    fn document_start(&mut self, _: &DocumentStart, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_document_start(false);
        self.moved(cx);
    }
    fn document_end(&mut self, _: &DocumentEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_document_end(false);
        self.moved(cx);
    }
    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.select_all();
        self.moved(cx);
    }
    fn newline(&mut self, _: &Newline, _: &mut Window, cx: &mut Context<Self>) {
        self.edit(cx, |buffer| buffer.insert("\n"));
    }
    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        if !self.disabled {
            cx.emit(TextInputEvent::Submit);
        }
    }
    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let text = text.replace("\r\n", "\n");
            self.edit(cx, |buffer| buffer.insert(&text));
        }
    }
    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.buffer.selection().is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.buffer.selected_text().to_owned(),
            ));
        }
    }
    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if !self.buffer.selection().is_empty() && !self.disabled {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.buffer.selected_text().to_owned(),
            ));
            self.edit(cx, |buffer| buffer.insert(""));
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        let Some(index) = self
            .layout
            .as_ref()
            .map(|layout| layout.index_for(event.position))
        else {
            return;
        };
        self.is_selecting = true;
        if event.modifiers.shift {
            self.buffer.select_to(index);
        } else if event.click_count >= 2 {
            let start = self
                .buffer
                .previous_word_start(self.buffer.next_boundary(index));
            let end = self.buffer.next_word_end(start);
            self.buffer.select_range(start..end);
        } else {
            self.buffer.move_to(index);
        }
        self.moved(cx);
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting
            && let Some(index) = self
                .layout
                .as_ref()
                .map(|layout| layout.index_for(event.position))
        {
            self.buffer.select_to(index);
            self.moved(cx);
        }
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.buffer.range_from_utf16(&range_utf16);
        actual_range.replace(self.buffer.range_to_utf16(&range));
        Some(self.buffer.text()[range].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.buffer.range_to_utf16(&self.buffer.selection()),
            reversed: self.buffer.is_reversed(),
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.buffer
            .marked()
            .map(|range| self.buffer.range_to_utf16(&range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.buffer.unmark();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16.map(|range| self.buffer.range_from_utf16(&range));
        self.edit(cx, |buffer| buffer.replace(range, new_text));
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16.map(|range| self.buffer.range_from_utf16(&range));
        // The selection is relative to the new text, which is not in the buffer yet.
        let selected = new_selected_range_utf16.map(|selected| {
            let marked = EditBuffer::new(new_text);
            marked.range_from_utf16(&selected)
        });
        self.edit(cx, |buffer| {
            buffer.replace_and_mark(range, new_text, selected)
        });
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.layout.as_ref()?;
        let range = self.buffer.range_from_utf16(&range_utf16);
        let start = layout.position_for(range.start);
        let end = layout.position_for(range.end);
        Some(Bounds::from_corners(
            element_bounds.origin + start,
            element_bounds.origin + point(end.x.max(start.x), end.y + layout.line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let index = self.layout.as_ref()?.index_for(point);
        Some(self.buffer.offset_to_utf16(index))
    }
}

/// Paints the buffer; lives inside [`TextInput`]'s render tree.
struct TextElement {
    input: Entity<TextInput>,
}

struct Prepaint {
    lines: Vec<(usize, Pixels, WrappedLine)>,
    selections: Vec<PaintQuad>,
    cursor: Option<PaintQuad>,
    line_height: Pixels,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// The text, runs, and color to shape this frame.
fn display(input: &TextInput, window: &Window) -> (SharedString, Vec<TextRun>) {
    let style = window.text_style();
    let (text, color): (SharedString, _) = if input.buffer.is_empty() {
        (input.placeholder.clone(), theme::text_muted())
    } else {
        (input.buffer.text().to_owned().into(), style.color)
    };
    let run = TextRun {
        len: text.len(),
        font: style.font(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let runs = match input.buffer.marked().filter(|_| !input.buffer.is_empty()) {
        Some(marked) => vec![
            TextRun {
                len: marked.start,
                ..run.clone()
            },
            TextRun {
                len: marked.end - marked.start,
                underline: Some(UnderlineStyle {
                    color: Some(run.color),
                    thickness: px(1.),
                    wavy: false,
                }),
                ..run.clone()
            },
            TextRun {
                len: text.len() - marked.end,
                ..run
            },
        ]
        .into_iter()
        .filter(|run| run.len > 0)
        .collect(),
        None => vec![run],
    };
    (text, runs)
}

fn shape(
    text: SharedString,
    runs: &[TextRun],
    width: Option<Pixels>,
    window: &Window,
) -> Vec<WrappedLine> {
    let font_size = window.text_style().font_size.to_pixels(window.rem_size());
    window
        .text_system()
        .shape_text(text, font_size, runs, width, None)
        .map(|lines| lines.into_vec())
        .unwrap_or_default()
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let (text, runs) = display(self.input.read(cx), window);
        let line_height = window.line_height();
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let layout_id =
            window.request_measured_layout(style, move |known, available, window, _| {
                let width = known.width.or(match available.width {
                    gpui::AvailableSpace::Definite(width) => Some(width),
                    _ => None,
                });
                let height = shape(text.clone(), &runs, width, window)
                    .iter()
                    .map(|line| line.size(line_height).height)
                    .fold(px(0.), |total, height| total + height)
                    .max(line_height);
                size(width.unwrap_or(px(0.)), height)
            });
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let (text, runs) = display(input, window);
        let line_height = window.line_height();
        let shaped = shape(text, &runs, Some(bounds.size.width), window);
        let starts = if input.buffer.is_empty() {
            vec![0; shaped.len()]
        } else {
            input
                .buffer
                .line_ranges()
                .into_iter()
                .map(|range| range.start)
                .collect()
        };
        let mut top = px(0.);
        let mut lines = Vec::with_capacity(shaped.len());
        for (line, start) in shaped.into_iter().zip(starts) {
            let height = line.size(line_height).height;
            lines.push((start, top, line));
            top += height;
        }
        let layout = PaintedLayout {
            bounds,
            line_height,
            lines: lines.clone(),
        };

        let selection = input.buffer.selection();
        let mut selections = Vec::new();
        if !selection.is_empty() && !input.buffer.is_empty() {
            for (start, _, line) in &lines {
                let line_range = *start..*start + line.len();
                let from = selection.start.max(line_range.start);
                let to = selection.end.min(line_range.end);
                // Selected newlines get a sliver so empty lines show as selected.
                let includes_newline =
                    selection.end > line_range.end && selection.start <= line_range.end;
                if from > to || (from == to && !includes_newline) {
                    continue;
                }
                let a = layout.position_for(from);
                let b = layout.position_for(to);
                let row_end = bounds.size.width;
                let quad = |x0: Pixels, x1: Pixels, y: Pixels| {
                    fill(
                        Bounds::from_corners(
                            bounds.origin + point(x0, y),
                            bounds.origin + point(x1.max(x0 + px(4.)), y + line_height),
                        ),
                        theme::selection(),
                    )
                };
                if a.y == b.y {
                    selections.push(quad(a.x, b.x, a.y));
                } else {
                    selections.push(quad(a.x, row_end, a.y));
                    let mut y = a.y + line_height;
                    while y < b.y {
                        selections.push(quad(px(0.), row_end, y));
                        y += line_height;
                    }
                    selections.push(quad(px(0.), b.x, b.y));
                }
            }
        }
        let cursor = (selection.is_empty() && !input.disabled).then(|| {
            let caret = layout.position_for(input.buffer.cursor());
            fill(
                Bounds::new(bounds.origin + caret, size(px(1.5), line_height)),
                theme::text(),
            )
        });
        Prepaint {
            lines,
            selections,
            cursor,
            line_height,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        for selection in prepaint.selections.drain(..) {
            window.paint_quad(selection);
        }
        for (_, top, line) in &prepaint.lines {
            let origin = bounds.origin + point(px(0.), *top);
            if let Err(error) = line.paint(
                origin,
                prepaint.line_height,
                TextAlign::Left,
                Some(bounds),
                window,
                cx,
            ) {
                tracing::warn!(%error, "could not paint text");
            }
        }
        if focus_handle.is_focused(window)
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }
        let layout = PaintedLayout {
            bounds,
            line_height: prepaint.line_height,
            lines: std::mem::take(&mut prepaint.lines),
        };
        self.input
            .update(cx, |input, _| input.layout = Some(layout));
    }
}

impl Render for TextInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .w_full()
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_to_line_start))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::line_start))
            .on_action(cx.listener(Self::line_end))
            .on_action(cx.listener(Self::select_line_start))
            .on_action(cx.listener(Self::select_line_end))
            .on_action(cx.listener(Self::document_start))
            .on_action(cx.listener(Self::document_end))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(TextElement { input: cx.entity() })
    }
}
