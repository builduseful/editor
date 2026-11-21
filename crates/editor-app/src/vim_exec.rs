//! Execution of `VimAction`s against `EditorApp` (motions, operators, text objects,
//! linewise paste, visual mode, search, and split-pane navigation).

use editor_core::{
    next_grapheme_boundary, next_word_boundary, prev_grapheme_boundary, word_range_at,
};
use editor_platform::{AppCommand, Clipboard, GlyphRasterizer, PlatformHost};

use crate::vim::{
    FindCharKind, InsertEntry, TextObject, VimAction, VimMode, VimMotion, VimOperator,
};
use crate::EditorApp;

impl EditorApp {
    pub(crate) fn execute_vim_action(
        &mut self,
        action: VimAction,
        rasterizer: &mut dyn GlyphRasterizer,
        clipboard: &mut dyn Clipboard,
        host: &mut dyn PlatformHost,
    ) {
        match action {
            VimAction::Consumed => {
                self.mark_dirty_rect(self.layout.status_bar);
                host.request_redraw();
            }
            VimAction::PassThrough => {}
            VimAction::ExitToNormal => {
                self.anchor = self.caret;
                // Keep caret on last char of line if past content end (standard Vim feel).
                let (line, col) = self.doc.byte_to_line_col(self.caret);
                if let Some((ls, content_end, _)) = self.doc.line_byte_range(line) {
                    let content_len = (content_end - ls) as usize;
                    if col > 0 && col >= content_len && content_len > 0 {
                        let content = self.doc.line_content(line).unwrap_or_default();
                        let prev = prev_grapheme_boundary(&content, content_len);
                        self.caret = ls + prev as u64;
                        self.anchor = self.caret;
                    }
                }
                self.mark_dirty();
                host.request_redraw();
            }
            VimAction::Move {
                motion,
                count,
                extend_visual,
            } => {
                let target = self.resolve_vim_motion(motion, count, false, rasterizer);
                self.caret = target;
                if !extend_visual {
                    self.anchor = target;
                } else if self.vim.mode == VimMode::VisualLine {
                    self.expand_visual_line_selection();
                }
                let m = self.doc_metrics(rasterizer);
                self.viewport
                    .ensure_caret_visible(&mut self.doc, self.caret, m, self.framebuffer.scale);
                self.mark_dirty();
                host.request_redraw();
            }
            VimAction::OperatorMotion { op, motion, count } => {
                let start = self.caret;
                let end = self.resolve_vim_motion(motion, count, true, rasterizer);
                let (a, b) = if start <= end {
                    (start, end)
                } else {
                    (end, start)
                };
                if matches!(motion, VimMotion::Up | VimMotion::Down | VimMotion::FileTop | VimMotion::FileBottom) {
                    let (la, _) = self.doc.byte_to_line_col(a);
                    let (lb, _) = self.doc.byte_to_line_col(b);
                    self.apply_vim_line_range(op, la, lb, rasterizer, clipboard, host);
                } else if b > a {
                    self.vim.last_yank_linewise = false;
                    self.apply_vim_byte_range(op, a, b, rasterizer, clipboard, host);
                }
            }
            VimAction::OperatorLines { op, count } => {
                let (line, _) = self.doc.byte_to_line_col(self.caret);
                let max_line = self.doc.editable_line_count().saturating_sub(1);
                let end_line = (line + count.saturating_sub(1)).min(max_line);
                self.apply_vim_line_range(op, line, end_line, rasterizer, clipboard, host);
            }
            VimAction::OperatorTextObject { op, obj } => {
                if let Some((a, b)) = self.resolve_text_object(obj) {
                    self.vim.last_yank_linewise = false;
                    self.apply_vim_byte_range(op, a, b, rasterizer, clipboard, host);
                }
            }
            VimAction::VisualApply(op) => {
                if let Some((a, b)) = self.selection() {
                    let linewise = self.vim.mode == VimMode::VisualLine;
                    self.vim.last_yank_linewise = linewise;
                    if matches!(op, VimOperator::Indent | VimOperator::Outdent) {
                        let (la, _) = self.doc.byte_to_line_col(a);
                        let (lb, _) = self.doc.byte_to_line_col(b.saturating_sub(1).max(a));
                        self.apply_vim_line_range(op, la, lb, rasterizer, clipboard, host);
                    } else {
                        self.apply_vim_byte_range(op, a, b, rasterizer, clipboard, host);
                    }
                }
            }
            VimAction::EnterInsert { kind } => {
                self.anchor = self.caret;
                let (line, col) = self.doc.byte_to_line_col(self.caret);
                match kind {
                    InsertEntry::Before => {}
                    InsertEntry::After => {
                        if let Some((ls, content_end, _)) = self.doc.line_byte_range(line) {
                            let content = self.doc.line_content(line).unwrap_or_default();
                            let next = next_grapheme_boundary(&content, col.min(content.len()));
                            self.caret = (ls + next as u64).min(content_end);
                            self.anchor = self.caret;
                        }
                    }
                    InsertEntry::LineStartNonBlank => {
                        self.caret = self.first_non_blank_offset(line);
                        self.anchor = self.caret;
                    }
                    InsertEntry::LineEnd => {
                        if let Some((_, content_end, _)) = self.doc.line_byte_range(line) {
                            self.caret = content_end;
                            self.anchor = self.caret;
                        }
                    }
                    InsertEntry::OpenBelow => {
                        if let Some((_, content_end, _)) = self.doc.line_byte_range(line) {
                            let content = self.doc.line_content(line).unwrap_or_default();
                            let indent: String = content
                                .chars()
                                .take_while(|c| *c == ' ' || *c == '\t')
                                .collect();
                            let ins = format!("\n{indent}");
                            if let Ok(pos) = self.doc.edit(content_end, 0, &ins, self.now_ms, false)
                            {
                                self.caret = pos;
                                self.anchor = pos;
                                self.after_document_mutated(rasterizer, host);
                                return;
                            }
                        }
                    }
                    InsertEntry::OpenAbove => {
                        if let Some((ls, _, _)) = self.doc.line_byte_range(line) {
                            let content = self.doc.line_content(line).unwrap_or_default();
                            let indent: String = content
                                .chars()
                                .take_while(|c| *c == ' ' || *c == '\t')
                                .collect();
                            let ins = format!("{indent}\n");
                            if self.doc.edit(ls, 0, &ins, self.now_ms, false).is_ok() {
                                self.caret = ls + indent.len() as u64;
                                self.anchor = self.caret;
                                self.after_document_mutated(rasterizer, host);
                                return;
                            }
                        }
                    }
                    InsertEntry::SubstituteChar => {
                        if let Some((ls, content_end, _)) = self.doc.line_byte_range(line) {
                            if self.caret < content_end {
                                let content = self.doc.line_content(line).unwrap_or_default();
                                let next = next_grapheme_boundary(&content, col.min(content.len()));
                                let len = (ls + next as u64).saturating_sub(self.caret);
                                if len > 0 {
                                    let _ = self.doc.edit(self.caret, len, "", self.now_ms, false);
                                    self.after_document_mutated(rasterizer, host);
                                    return;
                                }
                            }
                        }
                    }
                    InsertEntry::ChangeToEnd => {
                        if let Some((_, content_end, _)) = self.doc.line_byte_range(line) {
                            if content_end > self.caret {
                                let del_len = content_end - self.caret;
                                let slice = String::from_utf8_lossy(
                                    &self.doc.read_range(self.caret, del_len as usize),
                                )
                                .into_owned();
                                clipboard.set_text(&slice);
                                self.vim.last_yank_linewise = false;
                                let _ = self.doc.edit(self.caret, del_len, "", self.now_ms, false);
                                self.after_document_mutated(rasterizer, host);
                                return;
                            }
                        }
                    }
                }
                let m = self.doc_metrics(rasterizer);
                self.viewport
                    .ensure_caret_visible(&mut self.doc, self.caret, m, self.framebuffer.scale);
                self.mark_dirty();
                host.request_redraw();
            }
            VimAction::DeleteChar { before, count } => {
                let (line, col) = self.doc.byte_to_line_col(self.caret);
                let Some((ls, content_end, _)) = self.doc.line_byte_range(line) else {
                    return;
                };
                let content = self.doc.line_content(line).unwrap_or_default();
                let (del_start, del_end) = if before {
                    let mut c = col.min(content.len());
                    for _ in 0..count {
                        c = prev_grapheme_boundary(&content, c);
                    }
                    (ls + c as u64, self.caret)
                } else {
                    let mut c = col.min(content.len());
                    for _ in 0..count {
                        c = next_grapheme_boundary(&content, c);
                    }
                    (self.caret, (ls + c as u64).min(content_end))
                };
                if del_end > del_start {
                    let len = (del_end - del_start) as usize;
                    let s = String::from_utf8_lossy(&self.doc.read_range(del_start, len))
                        .into_owned();
                    clipboard.set_text(&s);
                    self.vim.last_yank_linewise = false;
                    if let Ok(pos) = self.doc.edit(del_start, len as u64, "", self.now_ms, false) {
                        self.caret = pos;
                        self.anchor = pos;
                        self.after_document_mutated(rasterizer, host);
                    }
                }
            }
            VimAction::ReplaceChar(ch) => {
                let (line, col) = self.doc.byte_to_line_col(self.caret);
                let Some((ls, content_end, _)) = self.doc.line_byte_range(line) else {
                    return;
                };
                if self.caret < content_end {
                    let content = self.doc.line_content(line).unwrap_or_default();
                    let next = next_grapheme_boundary(&content, col.min(content.len()));
                    let len = (ls + next as u64).saturating_sub(self.caret);
                    let mut buf = [0u8; 4];
                    let rep = ch.encode_utf8(&mut buf);
                    if self.doc.edit(self.caret, len, rep, self.now_ms, false).is_ok() {
                        self.anchor = self.caret;
                        self.after_document_mutated(rasterizer, host);
                    }
                }
            }
            VimAction::JoinLines(count) => {
                let (line, _) = self.doc.byte_to_line_col(self.caret);
                let mut joined = false;
                for _ in 0..count {
                    let total = self.doc.editable_line_count();
                    if line + 1 >= total {
                        break;
                    }
                    let Some((_, content_end, full_end)) = self.doc.line_byte_range(line) else {
                        break;
                    };
                    let next_content = self.doc.line_content(line + 1).unwrap_or_default();
                    let lead_ws = next_content.len() - next_content.trim_start().len();
                    let remove_len = (full_end - content_end) + lead_ws as u64;
                    if self.doc.edit(content_end, remove_len, " ", self.now_ms, false).is_ok() {
                        self.caret = content_end;
                        self.anchor = content_end;
                        joined = true;
                    }
                }
                if joined {
                    self.after_document_mutated(rasterizer, host);
                }
            }
            VimAction::Paste { before, linewise } => {
                let Some(text) = clipboard.get_text() else {
                    return;
                };
                if text.is_empty() {
                    return;
                }
                let (line, col) = self.doc.byte_to_line_col(self.caret);
                if linewise || text.ends_with('\n') {
                    let payload = if text.ends_with('\n') {
                        text
                    } else {
                        format!("{text}\n")
                    };
                    if before {
                        if let Some((ls, _, _)) = self.doc.line_byte_range(line) {
                            if self.doc.edit(ls, 0, &payload, self.now_ms, false).is_ok() {
                                self.caret = self.first_non_blank_offset(line);
                                self.anchor = self.caret;
                                self.after_document_mutated(rasterizer, host);
                            }
                        }
                    } else if let Some((_, content_end, full_end)) = self.doc.line_byte_range(line) {
                        let (ins_at, ins_text) = if full_end > content_end {
                            (full_end, payload)
                        } else {
                            (
                                content_end,
                                format!("\n{}", payload.trim_end_matches('\n')),
                            )
                        };
                        if self.doc.edit(ins_at, 0, &ins_text, self.now_ms, false).is_ok() {
                            self.caret = self.first_non_blank_offset(line + 1);
                            self.anchor = self.caret;
                            self.after_document_mutated(rasterizer, host);
                        }
                    }
                } else {
                    let ins_at = if before {
                        self.caret
                    } else if let Some((ls, content_end, _)) = self.doc.line_byte_range(line) {
                        let content = self.doc.line_content(line).unwrap_or_default();
                        let next = next_grapheme_boundary(&content, col.min(content.len()));
                        (ls + next as u64).min(content_end)
                    } else {
                        self.caret
                    };
                    if let Ok(pos) = self.doc.edit(ins_at, 0, &text, self.now_ms, false) {
                        self.caret = pos.saturating_sub(1).max(ins_at);
                        self.anchor = self.caret;
                        self.after_document_mutated(rasterizer, host);
                    }
                }
            }
            VimAction::Undo => self.handle_command(AppCommand::Undo, rasterizer, clipboard, host),
            VimAction::Redo => self.handle_command(AppCommand::Redo, rasterizer, clipboard, host),
            VimAction::OpenFind => {
                self.handle_command(AppCommand::Find, rasterizer, clipboard, host)
            }
            VimAction::SearchNext(forward) => {
                self.jump_search_match(forward, rasterizer, host);
            }
            VimAction::SearchWordUnderCursor => {
                let (line, col) = self.doc.byte_to_line_col(self.caret);
                let content = self.doc.line_content(line).unwrap_or_default();
                let (ws, we) = word_range_at(&content, col.min(content.len()));
                if we > ws {
                    let word = content[ws..we].to_string();
                    self.find_bar.find_input.set_text(&word);
                    self.refresh_search();
                    self.jump_search_match(true, rasterizer, host);
                }
            }
            VimAction::CenterView => {
                let (line, _) = self.doc.byte_to_line_col(self.caret);
                let metrics = self.doc_metrics(rasterizer);
                let vis_lines = (self.viewport.layout.text.h as f32
                    / metrics.line_height.max(1) as f32)
                    .floor() as usize;
                self.viewport.scroll_top_line = line.saturating_sub(vis_lines / 2);
                self.viewport.scroll_sub_y_px = 0.0;
                self.mark_dirty_rect(self.layout.viewport);
                host.request_redraw();
            }
            VimAction::SplitRight => self.split_pane_with(None, host),
            VimAction::CloseSplit => self.request_close_active_pane(host),
            VimAction::SwapPanes => self.swap_panes(host),
            VimAction::FocusPaneLeft => self.focus_pane(0, host),
            VimAction::FocusPaneRight => self.focus_pane(1, host),
            VimAction::FocusOtherPane => {
                let next = if self.active_pane == 0 { 1 } else { 0 };
                self.focus_pane(next, host);
            }
        }
    }

    fn first_non_blank_offset(&mut self, line: usize) -> u64 {
        let Some((ls, _, _)) = self.doc.line_byte_range(line) else {
            return 0;
        };
        let content = self.doc.line_content(line).unwrap_or_default();
        let ws = content.len() - content.trim_start_matches([' ', '\t']).len();
        ls + ws as u64
    }

    fn expand_visual_line_selection(&mut self) {
        let (a, b) = if self.anchor <= self.caret {
            (self.anchor, self.caret)
        } else {
            (self.caret, self.anchor)
        };
        let (la, _) = self.doc.byte_to_line_col(a);
        let (lb, _) = self.doc.byte_to_line_col(b);
        let start = self.doc.line_byte_range(la).map(|r| r.0).unwrap_or(0);
        let end = self
            .doc
            .line_byte_range(lb)
            .map(|r| r.2)
            .unwrap_or_else(|| self.doc.len_bytes());
        if self.anchor <= self.caret {
            self.anchor = start;
            self.caret = end;
        } else {
            self.anchor = end;
            self.caret = start;
        }
    }

    fn resolve_vim_motion(
        &mut self,
        motion: VimMotion,
        count: usize,
        inclusive_op: bool,
        rasterizer: &mut dyn GlyphRasterizer,
    ) -> u64 {
        let mut pos = self.caret;
        match motion {
            VimMotion::Left => {
                self.preferred_x_px = None;
                let (line, mut col) = self.doc.byte_to_line_col(pos);
                let (ls, _, _) = self.doc.line_byte_range(line).unwrap_or((0, 0, 0));
                let content = self.doc.line_content(line).unwrap_or_default();
                for _ in 0..count {
                    col = prev_grapheme_boundary(&content, col.min(content.len()));
                }
                ls + col as u64
            }
            VimMotion::Right => {
                self.preferred_x_px = None;
                let (line, mut col) = self.doc.byte_to_line_col(pos);
                let (ls, content_end, _) = self.doc.line_byte_range(line).unwrap_or((0, 0, 0));
                let content = self.doc.line_content(line).unwrap_or_default();
                for _ in 0..count {
                    col = next_grapheme_boundary(&content, col.min(content.len()));
                }
                (ls + col as u64).min(content_end)
            }
            VimMotion::Up => {
                let m = self.doc_metrics(rasterizer);
                let saved = self.caret;
                self.move_vertical(-(count as isize), m);
                let res = self.caret;
                self.caret = saved;
                res
            }
            VimMotion::Down => {
                let m = self.doc_metrics(rasterizer);
                let saved = self.caret;
                self.move_vertical(count as isize, m);
                let res = self.caret;
                self.caret = saved;
                res
            }
            VimMotion::WordForward => {
                self.preferred_x_px = None;
                for _ in 0..count {
                    pos = self.move_word_right(pos);
                }
                pos
            }
            VimMotion::WordBackward => {
                self.preferred_x_px = None;
                for _ in 0..count {
                    pos = self.move_word_left(pos);
                }
                pos
            }
            VimMotion::WordEnd => {
                self.preferred_x_px = None;
                for _ in 0..count {
                    let (line, col) = self.doc.byte_to_line_col(pos);
                    let (ls, content_end, full_end) =
                        self.doc.line_byte_range(line).unwrap_or((0, 0, 0));
                    let content = self.doc.line_content(line).unwrap_or_default();
                    let cur = col.min(content.len());
                    let step = next_grapheme_boundary(&content, cur);
                    let mut next = next_word_boundary(&content, step);
                    if !inclusive_op && next > step {
                        next = prev_grapheme_boundary(&content, next);
                    }
                    pos = if step >= content.len() && full_end > content_end {
                        full_end
                    } else {
                        ls + next as u64
                    };
                }
                pos
            }
            VimMotion::LineStart => {
                self.preferred_x_px = None;
                let (line, _) = self.doc.byte_to_line_col(pos);
                self.doc.line_byte_range(line).map(|r| r.0).unwrap_or(0)
            }
            VimMotion::FirstNonBlank => {
                self.preferred_x_px = None;
                let (line, _) = self.doc.byte_to_line_col(pos);
                self.first_non_blank_offset(line)
            }
            VimMotion::LineEnd => {
                self.preferred_x_px = None;
                let (line, _) = self.doc.byte_to_line_col(pos);
                self.doc
                    .line_byte_range(line)
                    .map(|r| r.1)
                    .unwrap_or(pos)
            }
            VimMotion::FileTop => {
                self.preferred_x_px = None;
                let target_line = count.saturating_sub(1);
                self.doc.tree_mut().ensure_indexed_for_line(target_line + 16);
                let max_l = self.doc.editable_line_count().saturating_sub(1);
                self.first_non_blank_offset(target_line.min(max_l))
            }
            VimMotion::FileBottom => {
                self.preferred_x_px = None;
                if count > 1 {
                    let target_line = count.saturating_sub(1);
                    self.doc.tree_mut().ensure_indexed_for_line(target_line + 16);
                    let max_l = self.doc.editable_line_count().saturating_sub(1);
                    self.first_non_blank_offset(target_line.min(max_l))
                } else {
                    self.doc.tree_mut().ensure_fully_indexed();
                    let last = self.doc.editable_line_count().saturating_sub(1);
                    self.first_non_blank_offset(last)
                }
            }
            VimMotion::MatchingBracket => {
                self.preferred_x_px = None;
                if let Some((a, b)) = self.doc.find_matching_bracket(pos) {
                    let target = if a == pos || a + 1 == pos { b } else { a };
                    if inclusive_op && target > pos {
                        target + 1
                    } else {
                        target
                    }
                } else {
                    pos
                }
            }
            VimMotion::ParagraphPrev => {
                self.preferred_x_px = None;
                let (mut line, _) = self.doc.byte_to_line_col(pos);
                for _ in 0..count {
                    while line > 0 {
                        line -= 1;
                        if self.doc.line_content(line).unwrap_or_default().trim().is_empty() {
                            break;
                        }
                    }
                }
                self.doc.line_byte_range(line).map(|r| r.0).unwrap_or(0)
            }
            VimMotion::ParagraphNext => {
                self.preferred_x_px = None;
                let (mut line, _) = self.doc.byte_to_line_col(pos);
                let total = self.doc.editable_line_count().max(1);
                for _ in 0..count {
                    while line + 1 < total {
                        line += 1;
                        if self.doc.line_content(line).unwrap_or_default().trim().is_empty() {
                            break;
                        }
                    }
                }
                self.doc
                    .line_byte_range(line)
                    .map(|r| r.0)
                    .unwrap_or_else(|| self.doc.len_bytes())
            }
            VimMotion::HalfPageDown | VimMotion::HalfPageUp => {
                let m = self.doc_metrics(rasterizer);
                let half = ((self.viewport.layout.text.h as f32 / m.line_height.max(1) as f32)
                    * 0.5)
                    .max(5.0) as isize;
                let delta = if matches!(motion, VimMotion::HalfPageDown) {
                    half * count as isize
                } else {
                    -half * count as isize
                };
                let saved = self.caret;
                self.move_vertical(delta, m);
                let res = self.caret;
                self.caret = saved;
                res
            }
            VimMotion::FindChar { kind, ch } => {
                self.preferred_x_px = None;
                let (line, col) = self.doc.byte_to_line_col(pos);
                let Some((ls, _, _)) = self.doc.line_byte_range(line) else {
                    return pos;
                };
                let content = self.doc.line_content(line).unwrap_or_default();
                let cur = col.min(content.len());
                match kind {
                    FindCharKind::Forward | FindCharKind::TillForward => {
                        let start_scan = next_grapheme_boundary(&content, cur);
                        let mut found = None;
                        let mut rem = count;
                        for (rel, c) in content[start_scan..].char_indices() {
                            if c == ch {
                                rem -= 1;
                                if rem == 0 {
                                    found = Some(start_scan + rel);
                                    break;
                                }
                            }
                        }
                        if let Some(idx) = found {
                            let final_col = if matches!(kind, FindCharKind::TillForward) {
                                prev_grapheme_boundary(&content, idx)
                            } else if inclusive_op {
                                idx + ch.len_utf8()
                            } else {
                                idx
                            };
                            ls + final_col as u64
                        } else {
                            pos
                        }
                    }
                    FindCharKind::Backward | FindCharKind::TillBackward => {
                        let mut found = None;
                        let mut rem = count;
                        for (idx, c) in content[..cur].char_indices().rev() {
                            if c == ch {
                                rem -= 1;
                                if rem == 0 {
                                    found = Some(idx);
                                    break;
                                }
                            }
                        }
                        if let Some(idx) = found {
                            let final_col = if matches!(kind, FindCharKind::TillBackward) {
                                idx + ch.len_utf8()
                            } else {
                                idx
                            };
                            ls + final_col as u64
                        } else {
                            pos
                        }
                    }
                }
            }
        }
    }

    fn resolve_text_object(&mut self, obj: TextObject) -> Option<(u64, u64)> {
        let (line, col) = self.doc.byte_to_line_col(self.caret);
        let (ls, _, _) = self.doc.line_byte_range(line)?;
        let content = self.doc.line_content(line)?;
        let cur = col.min(content.len());
        match obj {
            TextObject::InnerWord => {
                let (ws, we) = word_range_at(&content, cur);
                (we > ws).then_some((ls + ws as u64, ls + we as u64))
            }
            TextObject::InnerPair(open, close) => {
                if open == close {
                    // Quote pair on current line: find enclosing quotes around `cur`, or next pair on line.
                    let positions: Vec<usize> = content
                        .char_indices()
                        .filter_map(|(i, c)| (c == open).then_some(i))
                        .collect();
                    for pair in positions.chunks_exact(2) {
                        let (a, b) = (pair[0], pair[1]);
                        if cur <= b {
                            return Some((ls + (a + open.len_utf8()) as u64, ls + b as u64));
                        }
                    }
                    None
                } else {
                    // Enclosing bracket pair on current line.
                    let mut depth = 0i32;
                    let mut open_idx = None;
                    for (i, c) in content[..=cur.min(content.len().saturating_sub(1))]
                        .char_indices()
                        .rev()
                    {
                        if c == close && i != cur {
                            depth += 1;
                        } else if c == open {
                            if depth == 0 {
                                open_idx = Some(i);
                                break;
                            }
                            depth -= 1;
                        }
                    }
                    let oi = open_idx?;
                    let mut d = 0i32;
                    for (rel, c) in content[oi + open.len_utf8()..].char_indices() {
                        if c == open {
                            d += 1;
                        } else if c == close {
                            if d == 0 {
                                return Some((
                                    ls + (oi + open.len_utf8()) as u64,
                                    ls + (oi + open.len_utf8() + rel) as u64,
                                ));
                            }
                            d -= 1;
                        }
                    }
                    None
                }
            }
        }
    }

    fn apply_vim_byte_range(
        &mut self,
        op: VimOperator,
        a: u64,
        b: u64,
        rasterizer: &mut dyn GlyphRasterizer,
        clipboard: &mut dyn Clipboard,
        host: &mut dyn PlatformHost,
    ) {
        let len = (b - a) as usize;
        let text = String::from_utf8_lossy(&self.doc.read_range(a, len)).into_owned();
        clipboard.set_text(&text);
        match op {
            VimOperator::Yank => {
                self.caret = a;
                self.anchor = a;
                self.set_notice(format!("Yanked {len} B"), false);
                self.mark_dirty();
                host.request_redraw();
            }
            VimOperator::Delete | VimOperator::Change => {
                if let Ok(pos) = self.doc.edit(a, len as u64, "", self.now_ms, false) {
                    self.caret = pos;
                    self.anchor = pos;
                    self.after_document_mutated(rasterizer, host);
                }
            }
            VimOperator::Indent | VimOperator::Outdent => {
                let (la, _) = self.doc.byte_to_line_col(a);
                let (lb, _) = self.doc.byte_to_line_col(b.saturating_sub(1).max(a));
                self.apply_vim_line_range(op, la, lb, rasterizer, clipboard, host);
            }
        }
    }

    fn apply_vim_line_range(
        &mut self,
        op: VimOperator,
        start_line: usize,
        end_line: usize,
        rasterizer: &mut dyn GlyphRasterizer,
        clipboard: &mut dyn Clipboard,
        host: &mut dyn PlatformHost,
    ) {
        let Some((start_b, _, _)) = self.doc.line_byte_range(start_line) else {
            return;
        };
        let Some((_, end_content_b, end_full_b)) = self.doc.line_byte_range(end_line) else {
            return;
        };
        match op {
            VimOperator::Yank => {
                self.vim.last_yank_linewise = true;
                let len = (end_full_b - start_b) as usize;
                let mut text =
                    String::from_utf8_lossy(&self.doc.read_range(start_b, len)).into_owned();
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                clipboard.set_text(&text);
                self.anchor = self.caret;
                let lines = end_line - start_line + 1;
                self.set_notice(format!("Yanked {lines} line{}", if lines == 1 { "" } else { "s" }), false);
                self.mark_dirty();
                host.request_redraw();
            }
            VimOperator::Delete => {
                self.vim.last_yank_linewise = true;
                let (del_s, del_e) = if end_full_b > start_b {
                    (start_b, end_full_b)
                } else {
                    (start_b, end_content_b)
                };
                let len = (del_e - del_s) as usize;
                let mut text =
                    String::from_utf8_lossy(&self.doc.read_range(del_s, len)).into_owned();
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                clipboard.set_text(&text);
                if let Ok(pos) = self.doc.edit(del_s, len as u64, "", self.now_ms, false) {
                    let (new_l, _) = self.doc.byte_to_line_col(pos);
                    self.caret = self.first_non_blank_offset(new_l);
                    self.anchor = self.caret;
                    self.after_document_mutated(rasterizer, host);
                }
            }
            VimOperator::Change => {
                self.vim.last_yank_linewise = true;
                let content = self.doc.line_content(start_line).unwrap_or_default();
                let indent: String = content
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect();
                let len = (end_content_b - start_b) as usize;
                let text =
                    String::from_utf8_lossy(&self.doc.read_range(start_b, len)).into_owned();
                clipboard.set_text(&format!("{text}\n"));
                if let Ok(pos) = self
                    .doc
                    .edit(start_b, len as u64, &indent, self.now_ms, false)
                {
                    self.caret = pos;
                    self.anchor = pos;
                    self.after_document_mutated(rasterizer, host);
                }
            }
            VimOperator::Indent | VimOperator::Outdent => {
                let outdent = op == VimOperator::Outdent;
                for line in (start_line..=end_line).rev() {
                    let Some((ls, _, _)) = self.doc.line_byte_range(line) else {
                        continue;
                    };
                    if outdent {
                        let content = self.doc.line_content(line).unwrap_or_default();
                        let strip = if content.starts_with('\t') {
                            1
                        } else {
                            content.bytes().take(4).take_while(|b| *b == b' ').count()
                        };
                        if strip > 0 {
                            let _ = self.doc.edit(ls, strip as u64, "", self.now_ms, false);
                        }
                    } else {
                        let _ = self.doc.edit(ls, 0, "    ", self.now_ms, false);
                    }
                }
                self.caret = self.first_non_blank_offset(start_line);
                self.anchor = self.caret;
                self.after_document_mutated(rasterizer, host);
            }
        }
    }
}
