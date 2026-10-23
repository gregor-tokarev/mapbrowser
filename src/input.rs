// Adapted from GPUI 0.2.2 examples/input.rs.
// Copyright 2022 - 2025 Zed Industries, Inc. Licensed under Apache-2.0.
// See licenses/GPUI-Apache-2.0.txt.
// Modified for Mapbrowser: reusable field API, navigation events, scoped shortcuts,
// mouse focus, horizontal scrolling, themable styling, corrected IME selection,
// undo and redo, and pasting on one line.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, Global, GlobalElementId,
    Hsla, KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PaintQuad, Pixels, Point, ShapedLine, SharedString, Style, TextRun, UTF16Selection,
    UnderlineStyle, Window, actions, div, fill, point, prelude::*, px, relative, rgb, rgba, size,
};
use unicode_segmentation::*;

actions!(
    map_text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        ShowCharacterPalette,
        Paste,
        Cut,
        Copy,
        Submit,
        Cancel,
        SelectHome,
        SelectEnd,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        DeleteWordLeft,
        DeleteToStart,
    ]
);

pub struct TextInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
    scroll_x: Pixels,
    style: InputStyle,
    /// States ⌘Z goes back to, the latest last, and those ⇧⌘Z redoes.
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// The run of typing or deleting the last edit was part of, and where
    /// it left the caret: the next edit of that kind there joins it, so ⌘Z
    /// takes back a typed word or a held Backspace at once, as in AppKit.
    run: Option<(EditKind, usize)>,
    /// What the next `replace_text_in_range` is; the platform's text input
    /// calls it for typing.
    next_edit: EditKind,
    /// A password field: it shows a bullet per character and copies nothing.
    masked: bool,
}

/// The text and selection ⌘Z or ⇧⌘Z restores.
#[derive(Clone, Debug)]
struct Snapshot {
    content: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditKind {
    Typing,
    Deleting,
    /// Pasting, cutting and deleting a selection or a word undo one by one.
    Other,
}

/// Edits a field keeps for ⌘Z.
const UNDO_LIMIT: usize = 100;

/// What a password field shows for each character.
const MASK: char = '•';

/// Colours and metrics of the field; the defaults are the original dark style.
#[derive(Clone, Debug)]
pub struct InputStyle {
    pub text: Hsla,
    pub placeholder: Hsla,
    pub cursor: Hsla,
    pub selection: Hsla,
    pub font_size: Pixels,
    pub line_height: Pixels,
}

impl Default for InputStyle {
    fn default() -> Self {
        Self {
            text: rgb(0xe7e7ed).into(),
            placeholder: rgb(0x858593).into(),
            cursor: rgb(0xcacaf0).into(),
            selection: rgba(0x8681e860).into(),
            font_size: px(13.),
            line_height: px(22.),
        }
    }
}

#[derive(Clone, Debug)]
pub enum InputEvent {
    Submit(String),
    Cancel,
    /// The user edited the text; programmatic `set_text` doesn't emit this.
    Changed(String),
}

impl EventEmitter<InputEvent> for TextInput {}

struct InputBindings;
impl Global for InputBindings {}

/// Register shortcuts only in the input's focus context.
pub fn init(cx: &mut App) {
    if cx.has_global::<InputBindings>() {
        return;
    }
    cx.set_global(InputBindings);
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some("MapTextInput")),
        KeyBinding::new("delete", Delete, Some("MapTextInput")),
        KeyBinding::new("left", Left, Some("MapTextInput")),
        KeyBinding::new("right", Right, Some("MapTextInput")),
        KeyBinding::new("shift-left", SelectLeft, Some("MapTextInput")),
        KeyBinding::new("shift-right", SelectRight, Some("MapTextInput")),
        KeyBinding::new("alt-left", WordLeft, Some("MapTextInput")),
        KeyBinding::new("alt-right", WordRight, Some("MapTextInput")),
        KeyBinding::new("alt-shift-left", SelectWordLeft, Some("MapTextInput")),
        KeyBinding::new("alt-shift-right", SelectWordRight, Some("MapTextInput")),
        KeyBinding::new("alt-backspace", DeleteWordLeft, Some("MapTextInput")),
        KeyBinding::new("cmd-backspace", DeleteToStart, Some("MapTextInput")),
        KeyBinding::new("cmd-a", SelectAll, Some("MapTextInput")),
        KeyBinding::new("cmd-v", Paste, Some("MapTextInput")),
        KeyBinding::new("cmd-c", Copy, Some("MapTextInput")),
        KeyBinding::new("cmd-x", Cut, Some("MapTextInput")),
        KeyBinding::new("home", Home, Some("MapTextInput")),
        KeyBinding::new("end", End, Some("MapTextInput")),
        KeyBinding::new("cmd-left", Home, Some("MapTextInput")),
        KeyBinding::new("cmd-right", End, Some("MapTextInput")),
        KeyBinding::new("shift-home", SelectHome, Some("MapTextInput")),
        KeyBinding::new("shift-end", SelectEnd, Some("MapTextInput")),
        KeyBinding::new("cmd-shift-left", SelectHome, Some("MapTextInput")),
        KeyBinding::new("cmd-shift-right", SelectEnd, Some("MapTextInput")),
        KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, Some("MapTextInput")),
        KeyBinding::new("enter", Submit, Some("MapTextInput")),
        KeyBinding::new("escape", Cancel, Some("MapTextInput")),
    ]);
}

impl TextInput {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        init(cx);
        Self {
            focus_handle: cx.focus_handle(),
            content: "".into(),
            placeholder: "Search or enter address".into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            is_selecting: false,
            scroll_x: px(0.),
            style: InputStyle::default(),
            undo: Vec::new(),
            redo: Vec::new(),
            run: None,
            next_edit: EditKind::Typing,
            masked: false,
        }
    }

    pub fn set_style(&mut self, style: InputStyle, cx: &mut Context<Self>) {
        self.style = style;
        cx.notify();
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    /// Replaces the text, which ⌘Z then can't go back past: the address ⌘L
    /// starts with, a space's name, the last text found.
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.content = text.replace(['\r', '\n'], " ").into();
        self.selected_range = self.content.len()..self.content.len();
        self.selection_reversed = false;
        self.marked_range = None;
        self.scroll_x = px(0.);
        self.undo.clear();
        self.redo.clear();
        self.run = None;
        cx.notify();
    }

    pub fn set_masked(&mut self, masked: bool, cx: &mut Context<Self>) {
        self.masked = masked;
        cx.notify();
    }

    /// The offset in the text shown of `offset` in the content.
    fn shown_offset(&self, offset: usize) -> usize {
        if self.masked {
            self.content[..offset].chars().count() * MASK.len_utf8()
        } else {
            offset
        }
    }

    /// The offset in the content of `offset` in the text shown.
    fn content_offset(&self, offset: usize) -> usize {
        if !self.masked {
            return offset;
        }
        self.content
            .char_indices()
            .nth(offset / MASK.len_utf8())
            .map_or(self.content.len(), |(index, _)| index)
    }

    pub fn set_placeholder(&mut self, placeholder: &str, cx: &mut Context<Self>) {
        self.placeholder = placeholder.to_owned().into();
        cx.notify();
    }

    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus_handle);
    }

    pub fn select_all_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle);
        self.selected_range = 0..self.content.len();
        self.selection_reversed = false;
        cx.notify();
    }

    /// Prefills the field so typing replaces the whole text.
    pub fn set_text_and_select_all(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_text(text, cx);
        self.select_all_focus(window, cx);
    }

    fn emit_changed_since(&self, previous: &SharedString, cx: &mut Context<Self>) {
        if self.content != *previous {
            cx.emit(InputEvent::Changed(self.content.to_string()));
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            content: self.content.clone(),
            selected_range: self.selected_range.clone(),
            selection_reversed: self.selection_reversed,
        }
    }

    /// Before an edit of `kind` to `range`: keeps the text for ⌘Z, unless
    /// the edit continues the run of typing or deleting where it ended.
    fn begin_edit(&mut self, kind: EditKind, range: &Range<usize>) {
        let continues = match (kind, self.run) {
            (EditKind::Typing, Some((EditKind::Typing, caret))) => {
                self.selected_range.is_empty()
                    && *range == self.selected_range
                    && range.start == caret
            }
            // Backspace takes the character before the caret, Delete the
            // one after it.
            (EditKind::Deleting, Some((EditKind::Deleting, caret))) => {
                range.end == caret || range.start == caret
            }
            _ => false,
        };
        if !continues {
            self.undo.push(self.snapshot());
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
    }

    /// After an edit of `kind`: where its run goes on.
    fn end_edit(&mut self, kind: EditKind) {
        self.run = (kind != EditKind::Other).then(|| (kind, self.cursor_offset()));
    }

    /// ⌘Z, and Edit > Undo, while the field has focus. Neither reaches the
    /// page then, even with nothing left to undo.
    fn undo(&mut self, _: &crate::Undo, _: &mut Window, cx: &mut Context<Self>) {
        if self.marked_range.is_none()
            && let Some(snapshot) = self.undo.pop()
        {
            self.redo.push(self.snapshot());
            self.restore(snapshot, cx);
        }
    }

    fn redo(&mut self, _: &crate::Redo, _: &mut Window, cx: &mut Context<Self>) {
        if self.marked_range.is_none()
            && let Some(snapshot) = self.redo.pop()
        {
            self.undo.push(self.snapshot());
            self.restore(snapshot, cx);
        }
    }

    fn restore(&mut self, snapshot: Snapshot, cx: &mut Context<Self>) {
        let previous = std::mem::replace(&mut self.content, snapshot.content);
        self.selected_range = snapshot.selected_range;
        self.selection_reversed = snapshot.selection_reversed;
        self.run = None;
        self.emit_changed_since(&previous, cx);
        cx.notify();
    }

    /// Replaces the selection with `text` in an edit of `kind`.
    fn edit_selection(
        &mut self,
        kind: EditKind,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.next_edit = kind;
        self.replace_text_in_range(None, text, window, cx);
    }

    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        if self.marked_range.is_none() {
            cx.emit(InputEvent::Submit(self.content.to_string()));
        }
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        if self.marked_range.take().is_some() {
            cx.notify();
        } else {
            cx.emit(InputEvent::Cancel);
        }
    }

    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(0, cx);
    }

    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.content.len(), cx);
    }

    /// Where ⌥← goes. A password is one word, as in AppKit's secure
    /// fields: moving by its words would show where its spaces are.
    fn previous_word(&self) -> usize {
        if self.masked {
            return 0;
        }
        self.content[..self.cursor_offset()]
            .unicode_word_indices()
            .next_back()
            .map_or(0, |(offset, _)| offset)
    }

    fn next_word(&self) -> usize {
        if self.masked {
            return self.content.len();
        }
        let cursor = self.cursor_offset();
        self.content[cursor..]
            .unicode_word_indices()
            .next()
            .map_or(self.content.len(), |(offset, word)| {
                cursor + offset + word.len()
            })
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.previous_word(), cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.next_word(), cx);
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_word(), cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_word(), cx);
    }

    fn delete_word_left(
        &mut self,
        _: &DeleteWordLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_word(), cx);
        }
        self.edit_selection(EditKind::Other, "", window, cx);
    }

    fn delete_to_start(&mut self, _: &DeleteToStart, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(0, cx);
        }
        self.edit_selection(EditKind::Other, "", window, cx);
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        let kind = self.deletion_kind();
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx)
        }
        self.edit_selection(kind, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        let kind = self.deletion_kind();
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx)
        }
        self.edit_selection(kind, "", window, cx)
    }

    /// Single characters deleted one after another undo together; a
    /// deleted selection on its own.
    fn deletion_kind(&self) -> EditKind {
        if self.selected_range.is_empty() {
            EditKind::Deleting
        } else {
            EditKind::Other
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        cx.stop_propagation();
        self.is_selecting = true;
        if event.click_count >= 3 {
            self.select_all_focus(window, cx);
            return;
        }
        // A password's words aren't shown, so a double click takes it all.
        if event.click_count == 2 && self.masked {
            self.select_all_focus(window, cx);
            return;
        }
        if event.click_count == 2 {
            let offset = self.index_for_mouse_position(event.position);
            if let Some((start, segment)) = self
                .content
                .split_word_bound_indices()
                .find(|(start, segment)| offset >= *start && offset < start + segment.len())
            {
                self.selected_range = start..start + segment.len();
                self.selection_reversed = false;
                cx.notify();
                return;
            }
        }

        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx)
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.edit_selection(EditKind::Other, &one_line(&text), window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() && !self.masked {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }
    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() && !self.masked {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.edit_selection(EditKind::Other, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }

        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        self.content_offset(line.closest_index_for_x(position.x - bounds.left() + self.scroll_x))
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;

        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }

        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;

        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }

        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.marked_range = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        let kind = std::mem::replace(&mut self.next_edit, EditKind::Typing);
        // Committed composed text belongs to the edit its composition began.
        if self.marked_range.is_none() && !(range.is_empty() && new_text.is_empty()) {
            self.begin_edit(kind, &range);
        }
        let previous = self.content.clone();
        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.selection_reversed = false;
        self.marked_range.take();
        self.end_edit(kind);
        self.emit_changed_since(&previous, cx);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        // A composition (dead keys, input methods) is one edit, from its
        // first marked text until it is committed.
        if self.marked_range.is_none() {
            self.begin_edit(EditKind::Typing, &range);
        }
        let previous = self.content.clone();
        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|selected| {
                let to_utf8 = |offset: usize| {
                    let mut utf16 = 0;
                    let mut utf8 = 0;
                    for ch in new_text.chars() {
                        if utf16 >= offset {
                            break;
                        }
                        utf16 += ch.len_utf16();
                        utf8 += ch.len_utf8();
                    }
                    utf8
                };
                range.start + to_utf8(selected.start)..range.start + to_utf8(selected.end)
            })
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.selection_reversed = false;
        self.emit_changed_since(&previous, cx);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let last_layout = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        Some(Bounds::from_corners(
            point(
                bounds.left() + last_layout.x_for_index(self.shown_offset(range.start))
                    - self.scroll_x,
                bounds.top(),
            ),
            point(
                bounds.left() + last_layout.x_for_index(self.shown_offset(range.end))
                    - self.scroll_x,
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let line_point = self.last_bounds?.localize(&point)?;
        let last_layout = self.last_layout.as_ref()?;

        if self.content.is_empty() {
            return Some(0);
        }
        let utf8_index =
            self.content_offset(last_layout.closest_index_for_x(line_point.x + self.scroll_x));
        Some(self.offset_to_utf16(utf8_index))
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
    scroll_x: Pixels,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let selected_range = input.shown_offset(input.selected_range.start)
            ..input.shown_offset(input.selected_range.end);
        let cursor = input.shown_offset(input.cursor_offset());
        let style = window.text_style();

        let (display_text, text_color) = if content.is_empty() {
            (input.placeholder.clone(), input.style.placeholder)
        } else if input.masked {
            let bullets = MASK.to_string().repeat(content.chars().count());
            (bullets.into(), style.color)
        } else {
            (content, style.color)
        };

        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let marked_range = input
            .marked_range
            .as_ref()
            .map(|range| input.shown_offset(range.start)..input.shown_offset(range.end));
        let runs = if let Some(marked_range) = marked_range.as_ref() {
            vec![
                TextRun {
                    len: marked_range.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked_range.end - marked_range.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display_text.len() - marked_range.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect()
        } else {
            vec![run]
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);

        let cursor_pos = line.x_for_index(cursor);
        let visible_width = (bounds.size.width - px(2.)).max(px(0.));
        let scroll_x = if input.focus_handle.is_focused(window) {
            let mut scroll = input.scroll_x;
            if cursor_pos < scroll {
                scroll = cursor_pos;
            } else if cursor_pos > scroll + visible_width {
                scroll = cursor_pos - visible_width;
            }
            scroll.min((line.width - visible_width).max(px(0.)))
        } else {
            px(0.)
        };
        let (selection, cursor) = if selected_range.is_empty() {
            (
                None,
                Some(fill(
                    Bounds::new(
                        point(bounds.left() + cursor_pos - scroll_x, bounds.top()),
                        size(px(1.5), bounds.bottom() - bounds.top()),
                    ),
                    input.style.cursor,
                )),
            )
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(
                            bounds.left() + line.x_for_index(selected_range.start) - scroll_x,
                            bounds.top(),
                        ),
                        point(
                            bounds.left() + line.x_for_index(selected_range.end) - scroll_x,
                            bounds.bottom(),
                        ),
                    ),
                    input.style.selection,
                )),
                None,
            )
        };
        PrepaintState {
            line: Some(line),
            cursor,
            selection,
            scroll_x,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
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
        let line = prepaint.line.take().unwrap();
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if focus_handle.is_focused(window)
                && let Some(selection) = prepaint.selection.take()
            {
                window.paint_quad(selection);
            }
            let origin = point(bounds.left() - prepaint.scroll_x, bounds.top());
            let _ = line.paint(origin, window.line_height(), window, cx);
            if focus_handle.is_focused(window)
                && let Some(cursor) = prepaint.cursor.take()
            {
                window.paint_quad(cursor);
            }
        });

        self.input.update(cx, |input, _cx| {
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
            input.scroll_x = prepaint.scroll_x;
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .key_context("MapTextInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::show_character_palette))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_to_start))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .text_color(self.style.text)
            .line_height(self.style.line_height)
            .text_size(self.style.font_size)
            .child(TextElement { input: cx.entity() })
    }
}

/// Pasted text on one line. Line breaks go, with the spaces around them,
/// when the lines then make one address, as a link wrapped by a mail or
/// chat app does ("https://example.com/very/long/", "path/to/page");
/// between words they become spaces.
fn one_line(text: &str) -> String {
    if !text.contains(['\r', '\n']) {
        return text.to_owned();
    }
    let lines: Vec<&str> = text
        .split(['\r', '\n'])
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let joined = lines.concat();
    if lines.len() > 1
        && !joined.contains(char::is_whitespace)
        && !crate::model::is_search_query(&joined)
    {
        joined
    } else {
        lines.join(" ")
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
