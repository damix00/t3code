//! The composer's text model, independent of rendering: content, selection, IME marked text,
//! and the cursor movements the keymap binds. Offsets are UTF-8 byte offsets on grapheme
//! boundaries; the platform input layer speaks UTF-16, so conversions live here too.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditBuffer {
    text: String,
    selection: Range<usize>,
    /// Whether the cursor sits at the selection's start.
    reversed: bool,
    /// Text the IME is still composing.
    marked: Option<Range<usize>>,
}

/// Rough character classes for word movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Space,
    Word,
    Punctuation,
}

fn class(c: char) -> CharClass {
    if c.is_whitespace() {
        CharClass::Space
    } else if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else {
        CharClass::Punctuation
    }
}

impl EditBuffer {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let end = text.len();
        Self {
            text,
            selection: end..end,
            reversed: false,
            marked: None,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Whether there is nothing worth sending.
    pub fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }

    pub fn selection(&self) -> Range<usize> {
        self.selection.clone()
    }

    pub fn is_reversed(&self) -> bool {
        self.reversed
    }

    pub fn marked(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    pub fn cursor(&self) -> usize {
        if self.reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    /// Where a shift-movement keeps the selection anchored.
    fn anchor(&self) -> usize {
        if self.reversed {
            self.selection.end
        } else {
            self.selection.start
        }
    }

    pub fn selected_text(&self) -> &str {
        &self.text[self.selection.clone()]
    }

    /// Replaces everything and puts the cursor at the end.
    pub fn set_text(&mut self, text: impl Into<String>) {
        *self = Self::new(text);
    }

    /// Clears the buffer and returns what it held.
    pub fn take(&mut self) -> String {
        std::mem::take(self).text
    }

    fn clamp(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.text.len());
        while !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    }

    pub fn move_to(&mut self, offset: usize) {
        let offset = self.clamp(offset);
        self.selection = offset..offset;
        self.reversed = false;
    }

    /// Moves the cursor end of the selection, keeping the anchor.
    pub fn select_to(&mut self, offset: usize) {
        let offset = self.clamp(offset);
        let anchor = self.anchor();
        if offset < anchor {
            self.selection = offset..anchor;
            self.reversed = true;
        } else {
            self.selection = anchor..offset;
            self.reversed = false;
        }
    }

    /// Moves or extends, as shift decides.
    pub fn go_to(&mut self, offset: usize, extend: bool) {
        if extend {
            self.select_to(offset)
        } else {
            self.move_to(offset)
        }
    }

    pub fn select_all(&mut self) {
        self.selection = 0..self.text.len();
        self.reversed = false;
    }

    pub fn select_range(&mut self, range: Range<usize>) {
        let start = self.clamp(range.start);
        let end = self.clamp(range.end);
        self.selection = start.min(end)..start.max(end);
        self.reversed = false;
    }

    pub fn previous_boundary(&self, offset: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .rev()
            .find_map(|(index, _)| (index < offset).then_some(index))
            .unwrap_or(0)
    }

    pub fn next_boundary(&self, offset: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .find_map(|(index, _)| (index > offset).then_some(index))
            .unwrap_or(self.text.len())
    }

    /// The start of the word before `offset`, skipping whitespace first.
    pub fn previous_word_start(&self, offset: usize) -> usize {
        let before: Vec<(usize, char)> = self.text[..self.clamp(offset)].char_indices().collect();
        let mut index = before.len();
        while index > 0 && class(before[index - 1].1) == CharClass::Space {
            index -= 1;
        }
        let Some(&(_, c)) = index.checked_sub(1).and_then(|i| before.get(i)) else {
            return 0;
        };
        let target = class(c);
        while index > 0 && class(before[index - 1].1) == target {
            index -= 1;
        }
        before.get(index).map_or(0, |&(offset, _)| offset)
    }

    /// The end of the word after `offset`, skipping whitespace first.
    pub fn next_word_end(&self, offset: usize) -> usize {
        let start = self.clamp(offset);
        let mut chars = self.text[start..].char_indices().peekable();
        while chars
            .next_if(|&(_, c)| class(c) == CharClass::Space)
            .is_some()
        {}
        let Some(&(_, first)) = chars.peek() else {
            return self.text.len();
        };
        let target = class(first);
        while chars.next_if(|&(_, c)| class(c) == target).is_some() {}
        chars
            .peek()
            .map_or(self.text.len(), |&(index, _)| start + index)
    }

    pub fn line_start(&self, offset: usize) -> usize {
        let offset = self.clamp(offset);
        self.text[..offset].rfind('\n').map_or(0, |index| index + 1)
    }

    pub fn line_end(&self, offset: usize) -> usize {
        let offset = self.clamp(offset);
        self.text[offset..]
            .find('\n')
            .map_or(self.text.len(), |index| offset + index)
    }

    pub fn move_left(&mut self, extend: bool) {
        if !extend && !self.selection.is_empty() {
            self.move_to(self.selection.start);
        } else {
            self.go_to(self.previous_boundary(self.cursor()), extend);
        }
    }

    pub fn move_right(&mut self, extend: bool) {
        if !extend && !self.selection.is_empty() {
            self.move_to(self.selection.end);
        } else {
            self.go_to(self.next_boundary(self.cursor()), extend);
        }
    }

    pub fn move_word_left(&mut self, extend: bool) {
        self.go_to(self.previous_word_start(self.cursor()), extend);
    }

    pub fn move_word_right(&mut self, extend: bool) {
        self.go_to(self.next_word_end(self.cursor()), extend);
    }

    pub fn move_line_start(&mut self, extend: bool) {
        self.go_to(self.line_start(self.cursor()), extend);
    }

    pub fn move_line_end(&mut self, extend: bool) {
        self.go_to(self.line_end(self.cursor()), extend);
    }

    pub fn move_document_start(&mut self, extend: bool) {
        self.go_to(0, extend);
    }

    pub fn move_document_end(&mut self, extend: bool) {
        self.go_to(self.text.len(), extend);
    }

    /// Replaces `range` (or the marked text, or the selection) and puts the cursor after it.
    pub fn replace(&mut self, range: Option<Range<usize>>, new_text: &str) {
        let range = range
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selection.clone());
        let start = self.clamp(range.start);
        let end = self.clamp(range.end).max(start);
        self.text.replace_range(start..end, new_text);
        let cursor = start + new_text.len();
        self.selection = cursor..cursor;
        self.reversed = false;
        self.marked = None;
    }

    /// Types `text` over the selection.
    pub fn insert(&mut self, text: &str) {
        self.replace(None, text);
    }

    /// Replaces `range` with IME composition text and marks it.
    pub fn replace_and_mark(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        selected: Option<Range<usize>>,
    ) {
        let range = range
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selection.clone());
        let start = self.clamp(range.start);
        let end = self.clamp(range.end).max(start);
        self.text.replace_range(start..end, new_text);
        self.marked = (!new_text.is_empty()).then(|| start..start + new_text.len());
        let selection = selected
            .map(|selected| start + selected.start..start + selected.end)
            .unwrap_or(start + new_text.len()..start + new_text.len());
        self.selection = self.clamp(selection.start)..self.clamp(selection.end);
        self.reversed = false;
    }

    pub fn unmark(&mut self) {
        self.marked = None;
    }

    pub fn backspace(&mut self) {
        if self.selection.is_empty() {
            self.select_to(self.previous_boundary(self.cursor()));
        }
        self.replace(Some(self.selection.clone()), "");
    }

    pub fn delete(&mut self) {
        if self.selection.is_empty() {
            self.select_to(self.next_boundary(self.cursor()));
        }
        self.replace(Some(self.selection.clone()), "");
    }

    pub fn delete_word_left(&mut self) {
        if self.selection.is_empty() {
            self.select_to(self.previous_word_start(self.cursor()));
        }
        self.replace(Some(self.selection.clone()), "");
    }

    pub fn delete_to_line_start(&mut self) {
        if self.selection.is_empty() {
            let cursor = self.cursor();
            let start = self.line_start(cursor);
            // At a line start, join with the previous line instead of doing nothing.
            self.select_to(if start == cursor {
                self.previous_boundary(cursor)
            } else {
                start
            });
        }
        self.replace(Some(self.selection.clone()), "");
    }

    pub fn offset_to_utf16(&self, offset: usize) -> usize {
        self.text[..self.clamp(offset)].encode_utf16().count()
    }

    pub fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf16 = 0;
        for (index, c) in self.text.char_indices() {
            if utf16 >= offset {
                return index;
            }
            utf16 += c.len_utf16();
        }
        self.text.len()
    }

    pub fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    pub fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    /// The logical lines (split on `\n`) as byte ranges, always at least one.
    pub fn line_ranges(&self) -> Vec<Range<usize>> {
        let mut ranges = Vec::new();
        let mut start = 0;
        for (index, _) in self.text.match_indices('\n') {
            ranges.push(start..index);
            start = index + 1;
        }
        ranges.push(start..self.text.len());
        ranges
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use proptest::prelude::*;

    fn at(text: &str, cursor: usize) -> EditBuffer {
        let mut buffer = EditBuffer::new(text);
        buffer.move_to(cursor);
        buffer
    }

    #[test]
    fn typing_replaces_the_selection() {
        let mut buffer = EditBuffer::new("hello world");
        buffer.select_range(0..5);
        buffer.insert("goodbye");
        assert_eq!(buffer.text(), "goodbye world");
        assert_eq!(buffer.selection(), 7..7);
    }

    #[test]
    fn backspace_and_delete_respect_graphemes() {
        let mut buffer = EditBuffer::new("ae\u{301}👍🏽");
        buffer.backspace();
        assert_eq!(buffer.text(), "ae\u{301}");
        buffer.backspace();
        assert_eq!(buffer.text(), "a");
        buffer.backspace();
        buffer.backspace();
        assert_eq!(buffer.text(), "");
        let mut buffer = at("ab", 0);
        buffer.delete();
        assert_eq!(buffer.text(), "b");
        buffer.move_to(1);
        buffer.delete();
        assert_eq!(buffer.text(), "b");
    }

    #[test]
    fn arrows_collapse_a_selection_before_moving() {
        let mut buffer = EditBuffer::new("abcdef");
        buffer.select_range(2..4);
        buffer.move_left(false);
        assert_eq!(buffer.selection(), 2..2);
        buffer.select_range(2..4);
        buffer.move_right(false);
        assert_eq!(buffer.selection(), 4..4);
    }

    #[test]
    fn shift_movement_extends_from_the_anchor_in_both_directions() {
        let mut buffer = at("abcdef", 3);
        buffer.move_right(true);
        buffer.move_right(true);
        assert_eq!((buffer.selection(), buffer.is_reversed()), (3..5, false));
        buffer.move_left(true);
        buffer.move_left(true);
        buffer.move_left(true);
        assert_eq!((buffer.selection(), buffer.is_reversed()), (2..3, true));
        assert_eq!(buffer.cursor(), 2);
        assert_eq!(buffer.selected_text(), "c");
    }

    #[test]
    fn word_movement() {
        let text = "let  foo_bar = baz(1);";
        let buffer = EditBuffer::new(text);
        assert_eq!(buffer.next_word_end(0), 3);
        assert_eq!(buffer.next_word_end(3), 12);
        assert_eq!(buffer.next_word_end(12), 14);
        assert_eq!(buffer.next_word_end(18), 19);
        assert_eq!(buffer.next_word_end(text.len()), text.len());
        assert_eq!(buffer.previous_word_start(text.len()), 20);
        assert_eq!(buffer.previous_word_start(12), 5);
        assert_eq!(buffer.previous_word_start(5), 0);
        assert_eq!(buffer.previous_word_start(0), 0);
        assert_eq!(EditBuffer::new("   ").previous_word_start(3), 0);
    }

    #[test]
    fn word_and_line_deletion() {
        let mut buffer = EditBuffer::new("first line\nsecond word");
        buffer.delete_word_left();
        assert_eq!(buffer.text(), "first line\nsecond ");
        buffer.delete_to_line_start();
        assert_eq!(buffer.text(), "first line\n");
        buffer.delete_to_line_start();
        assert_eq!(buffer.text(), "first line");
        buffer.delete_to_line_start();
        assert_eq!(buffer.text(), "");
    }

    #[test]
    fn line_navigation() {
        let mut buffer = at("one\ntwo three\nfour", 6);
        assert_eq!(buffer.line_start(6), 4);
        assert_eq!(buffer.line_end(6), 13);
        buffer.move_line_end(true);
        assert_eq!(buffer.selected_text(), "o three");
        buffer.move_line_start(false);
        assert_eq!(buffer.cursor(), 4);
        buffer.move_document_end(true);
        assert_eq!(buffer.selected_text(), "two three\nfour");
        buffer.move_document_start(false);
        assert_eq!(buffer.cursor(), 0);
        assert_eq!(buffer.line_start(4), 4);
        assert_eq!(buffer.line_end(3), 3);
    }

    #[test]
    fn line_ranges_split_on_newlines() {
        assert_eq!(EditBuffer::new("").line_ranges(), vec![0..0]);
        assert_eq!(EditBuffer::new("a\n").line_ranges(), vec![0..1, 2..2]);
        assert_eq!(
            EditBuffer::new("ab\n\ncd").line_ranges(),
            vec![0..2, 3..3, 4..6]
        );
    }

    #[test]
    fn utf16_round_trips() {
        let buffer = EditBuffer::new("a😀é\nb");
        assert_eq!(buffer.offset_to_utf16(0), 0);
        assert_eq!(buffer.offset_to_utf16(1), 1);
        assert_eq!(buffer.offset_to_utf16(5), 3);
        assert_eq!(buffer.offset_to_utf16(7), 4);
        assert_eq!(buffer.offset_from_utf16(3), 5);
        assert_eq!(buffer.offset_from_utf16(4), 7);
        assert_eq!(buffer.offset_from_utf16(99), buffer.text().len());
        assert_eq!(buffer.range_from_utf16(&(1..3)), 1..5);
        assert_eq!(buffer.range_to_utf16(&(1..5)), 1..3);
        // Mid-character offsets clamp down.
        assert_eq!(buffer.offset_to_utf16(2), 1);
    }

    #[test]
    fn ime_composition_marks_then_commits() {
        let mut buffer = EditBuffer::new("x");
        buffer.replace_and_mark(None, "k", None);
        assert_eq!(buffer.marked(), Some(1..2));
        buffer.replace_and_mark(None, "ka", Some(2..2));
        assert_eq!(
            (buffer.text(), buffer.marked(), buffer.selection()),
            ("xka", Some(1..3), 3..3)
        );
        buffer.replace(None, "か");
        assert_eq!((buffer.text(), buffer.marked()), ("xか", None));
        buffer.replace_and_mark(None, "", None);
        assert_eq!(buffer.marked(), None);
        buffer.replace_and_mark(None, "a", None);
        buffer.unmark();
        assert_eq!(buffer.marked(), None);
    }

    #[test]
    fn take_and_blankness() {
        let mut buffer = EditBuffer::new(" \n\t");
        assert!(buffer.is_blank());
        assert!(!buffer.is_empty());
        buffer.set_text("hi");
        assert_eq!(buffer.cursor(), 2);
        assert_eq!(buffer.take(), "hi");
        assert!(buffer.is_empty());
        assert_eq!(buffer.selection(), 0..0);
    }

    #[test]
    #[allow(clippy::reversed_empty_ranges)] // a reversed range is one of the inputs under test
    fn out_of_range_offsets_clamp() {
        let mut buffer = EditBuffer::new("é");
        buffer.move_to(1);
        assert_eq!(buffer.cursor(), 0);
        buffer.move_to(99);
        assert_eq!(buffer.cursor(), 2);
        buffer.replace(Some(5..1), "x");
        assert_eq!(buffer.text(), "éx");
    }

    fn arbitrary_op() -> impl Strategy<Value = u8> {
        0u8..18
    }

    proptest! {
        /// No sequence of edits can leave the selection off a character boundary or out of bounds.
        #[test]
        fn edits_keep_offsets_valid(initial in "\\PC{0,12}", ops in prop::collection::vec((arbitrary_op(), "\\PC{0,3}", any::<bool>()), 0..40)) {
            let mut buffer = EditBuffer::new(initial);
            for (op, text, extend) in ops {
                match op {
                    0 => buffer.insert(&text),
                    1 => buffer.backspace(),
                    2 => buffer.delete(),
                    3 => buffer.move_left(extend),
                    4 => buffer.move_right(extend),
                    5 => buffer.move_word_left(extend),
                    6 => buffer.move_word_right(extend),
                    7 => buffer.move_line_start(extend),
                    8 => buffer.move_line_end(extend),
                    9 => buffer.select_all(),
                    10 => buffer.delete_word_left(),
                    11 => buffer.delete_to_line_start(),
                    12 => buffer.replace_and_mark(None, &text, None),
                    13 => buffer.unmark(),
                    14 => buffer.insert("\n"),
                    15 => buffer.move_document_start(extend),
                    16 => buffer.move_document_end(extend),
                    _ => { let len = buffer.text().len(); buffer.select_range(len / 3..len / 2) }
                }
                let selection = buffer.selection();
                prop_assert!(selection.start <= selection.end);
                prop_assert!(selection.end <= buffer.text().len());
                prop_assert!(buffer.text().is_char_boundary(selection.start));
                prop_assert!(buffer.text().is_char_boundary(selection.end));
                if let Some(marked) = buffer.marked() {
                    prop_assert!(marked.end <= buffer.text().len());
                }
                let cursor = buffer.cursor();
                prop_assert_eq!(buffer.offset_from_utf16(buffer.offset_to_utf16(cursor)), cursor);
            }
        }
    }
}
