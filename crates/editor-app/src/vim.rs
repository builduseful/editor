//! Core Neovim-compatible modal editing state machine (`Normal` / `Insert` / `Visual` / `V-Line`).
//!
//! Zero external dependencies; operates directly on `EditorApp` via `VimMotion` and
//! `VimAction` so undo coalescing, multi-line brackets, clipboard, and split panes
//! all share the exact same tested core primitives.

use editor_platform::{Key, Modifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VimMode {
    Normal,
    Insert,
    Visual,
    VisualLine,
}

impl VimMode {
    pub fn label(self) -> &'static str {
        match self {
            VimMode::Normal => "NORMAL",
            VimMode::Insert => "INSERT",
            VimMode::Visual => "VISUAL",
            VimMode::VisualLine => "V-LINE",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VimOperator {
    Delete,
    Change,
    Yank,
    Indent,
    Outdent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindCharKind {
    Forward,      // f
    Backward,     // F
    TillForward,  // t
    TillBackward, // T
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VimPending {
    None,
    /// Waiting for second key after `g` (e.g. `gg`).
    GPrefix,
    /// Waiting for second key after `z` (e.g. `zz`).
    ZPrefix,
    /// Waiting for second key after `Ctrl+W` (pane commands).
    WindowPrefix,
    /// Waiting for target character after `f`/`F`/`t`/`T`.
    FindChar(FindCharKind),
    /// Waiting for replacement character after `r`.
    ReplaceChar,
    /// Waiting for text object character after `i` (e.g. `iw`, `i"`, `i(`, `i{`, `i[`).
    InnerObject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VimMotion {
    Left,
    Right,
    Up,
    Down,
    WordForward,
    WordBackward,
    WordEnd,
    LineStart,
    FirstNonBlank,
    LineEnd,
    FileTop,
    FileBottom,
    MatchingBracket,
    ParagraphPrev,
    ParagraphNext,
    HalfPageDown,
    HalfPageUp,
    FindChar { kind: FindCharKind, ch: char },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextObject {
    InnerWord,
    InnerPair(char, char),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VimAction {
    /// Key was consumed with no document effect (e.g., accumulated count/prefix).
    Consumed,
    /// Not handled by Vim (e.g. Insert mode normal typing).
    PassThrough,
    Move {
        motion: VimMotion,
        count: usize,
        extend_visual: bool,
    },
    OperatorMotion {
        op: VimOperator,
        motion: VimMotion,
        count: usize,
    },
    OperatorLines {
        op: VimOperator,
        count: usize,
    },
    OperatorTextObject {
        op: VimOperator,
        obj: TextObject,
    },
    VisualApply(VimOperator),
    EnterInsert {
        kind: InsertEntry,
    },
    ExitToNormal,
    DeleteChar {
        before: bool,
        count: usize,
    },
    ReplaceChar(char),
    JoinLines(usize),
    Paste {
        before: bool,
        linewise: bool,
    },
    Undo,
    Redo,
    OpenFind,
    SearchNext(bool),
    SearchWordUnderCursor,
    CenterView,
    SplitRight,
    CloseSplit,
    SwapPanes,
    FocusPaneLeft,
    FocusPaneRight,
    FocusOtherPane,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertEntry {
    Before,
    After,
    LineStartNonBlank,
    LineEnd,
    OpenBelow,
    OpenAbove,
    SubstituteChar,
    ChangeToEnd,
}

pub struct VimState {
    pub enabled: bool,
    pub mode: VimMode,
    count: usize,
    operator: Option<VimOperator>,
    pending: VimPending,
    last_find: Option<(FindCharKind, char)>,
    pub last_yank_linewise: bool,
}

impl Default for VimState {
    fn default() -> Self {
        Self::new()
    }
}

impl VimState {
    pub fn new() -> Self {
        Self {
            enabled: false,
            mode: VimMode::Normal,
            count: 0,
            operator: None,
            pending: VimPending::None,
            last_find: None,
            last_yank_linewise: false,
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        self.reset_pending();
        self.mode = VimMode::Normal;
    }

    pub fn toggle(&mut self) -> bool {
        let next = !self.enabled;
        self.set_enabled(next);
        next
    }

    pub fn reset_pending(&mut self) {
        self.count = 0;
        self.operator = None;
        self.pending = VimPending::None;
    }

    pub fn uses_block_caret(&self) -> bool {
        self.enabled && self.mode != VimMode::Insert
    }

    /// Concise status bar badge (e.g. `"NORMAL"`, `"NORMAL 2d"`, `"INSERT"`, `"V-LINE"`).
    pub fn status_badge(&self) -> Option<String> {
        if !self.enabled {
            return None;
        }
        let mut s = self.mode.label().to_string();
        let mut suffix = String::new();
        if self.count > 0 {
            suffix.push_str(&self.count.to_string());
        }
        if let Some(op) = self.operator {
            suffix.push(match op {
                VimOperator::Delete => 'd',
                VimOperator::Change => 'c',
                VimOperator::Yank => 'y',
                VimOperator::Indent => '>',
                VimOperator::Outdent => '<',
            });
        }
        match self.pending {
            VimPending::None => {}
            VimPending::GPrefix => suffix.push('g'),
            VimPending::ZPrefix => suffix.push('z'),
            VimPending::WindowPrefix => suffix.push_str("^W"),
            VimPending::FindChar(k) => suffix.push(match k {
                FindCharKind::Forward => 'f',
                FindCharKind::Backward => 'F',
                FindCharKind::TillForward => 't',
                FindCharKind::TillBackward => 'T',
            }),
            VimPending::ReplaceChar => suffix.push('r'),
            VimPending::InnerObject => suffix.push('i'),
        }
        if !suffix.is_empty() {
            s.push(' ');
            s.push_str(&suffix);
        }
        Some(s)
    }

    fn take_count(&mut self) -> usize {
        let c = self.count.max(1);
        self.count = 0;
        c
    }

    fn dispatch_motion(&mut self, motion: VimMotion) -> VimAction {
        let count = self.take_count();
        self.pending = VimPending::None;
        if let Some(op) = self.operator.take() {
            if op == VimOperator::Change {
                self.mode = VimMode::Insert;
            }
            VimAction::OperatorMotion { op, motion, count }
        } else {
            let extend_visual = matches!(self.mode, VimMode::Visual | VimMode::VisualLine);
            VimAction::Move {
                motion,
                count,
                extend_visual,
            }
        }
    }

    /// Feed a key press into the Vim state machine when the document viewport is focused.
    pub fn handle_key(&mut self, key: Key, mods: Modifiers, text: Option<&str>) -> VimAction {
        if !self.enabled || mods.primary() || mods.alt {
            return VimAction::PassThrough;
        }

        // Insert mode: Esc or Ctrl+[ returns to Normal; everything else passes through to normal editing!
        if self.mode == VimMode::Insert {
            if key == Key::Escape || (mods.ctrl && matches!(key, Key::Char('['))) {
                self.mode = VimMode::Normal;
                self.reset_pending();
                return VimAction::ExitToNormal;
            }
            return VimAction::PassThrough;
        }

        // Escape cancels pending operators/counts or exits Visual mode.
        if key == Key::Escape || (mods.ctrl && matches!(key, Key::Char('['))) {
            let was_visual = self.mode != VimMode::Normal;
            self.mode = VimMode::Normal;
            self.reset_pending();
            return if was_visual {
                VimAction::ExitToNormal
            } else {
                VimAction::Consumed
            };
        }

        // Resolve the typed character (prefer `text` so shifted punctuation like `$`, `^`, `%`, `G` works accurately!).
        let ch_opt: Option<char> = if mods.ctrl {
            match key {
                Key::Char(c) => Some(c.to_ascii_lowercase()),
                _ => None,
            }
        } else if let Some(t) = text {
            let mut it = t.chars();
            match (it.next(), it.next()) {
                (Some(c), None) if !c.is_control() => Some(c),
                _ => match key {
                    Key::Char(c) => Some(if mods.shift { c.to_ascii_uppercase() } else { c }),
                    _ => None,
                },
            }
        } else {
            match key {
                Key::Char(c) => Some(if mods.shift { c.to_ascii_uppercase() } else { c }),
                _ => None,
            }
        };

        // Handle pending multi-key sequences first.
        match self.pending {
            VimPending::FindChar(kind) => {
                self.pending = VimPending::None;
                if let Some(ch) = ch_opt {
                    self.last_find = Some((kind, ch));
                    return self.dispatch_motion(VimMotion::FindChar { kind, ch });
                }
                self.reset_pending();
                return VimAction::Consumed;
            }
            VimPending::ReplaceChar => {
                self.pending = VimPending::None;
                self.reset_pending();
                return match ch_opt {
                    Some(ch) => VimAction::ReplaceChar(ch),
                    None => VimAction::Consumed,
                };
            }
            VimPending::InnerObject => {
                self.pending = VimPending::None;
                let op = self.operator.take();
                self.count = 0;
                if let (Some(op), Some(ch)) = (op, ch_opt) {
                    let obj = match ch {
                        'w' | 'W' => Some(TextObject::InnerWord),
                        '"' => Some(TextObject::InnerPair('"', '"')),
                        '\'' => Some(TextObject::InnerPair('\'', '\'')),
                        '`' => Some(TextObject::InnerPair('`', '`')),
                        '(' | ')' | 'b' => Some(TextObject::InnerPair('(', ')')),
                        '[' | ']' => Some(TextObject::InnerPair('[', ']')),
                        '{' | '}' | 'B' => Some(TextObject::InnerPair('{', '}')),
                        '<' | '>' => Some(TextObject::InnerPair('<', '>')),
                        _ => None,
                    };
                    if let Some(obj) = obj {
                        if op == VimOperator::Change {
                            self.mode = VimMode::Insert;
                        }
                        return VimAction::OperatorTextObject { op, obj };
                    }
                }
                return VimAction::Consumed;
            }
            VimPending::GPrefix => {
                self.pending = VimPending::None;
                if ch_opt == Some('g') {
                    return self.dispatch_motion(VimMotion::FileTop);
                }
                self.reset_pending();
                return VimAction::Consumed;
            }
            VimPending::ZPrefix => {
                self.pending = VimPending::None;
                self.reset_pending();
                if ch_opt == Some('z') {
                    return VimAction::CenterView;
                }
                return VimAction::Consumed;
            }
            VimPending::WindowPrefix => {
                self.pending = VimPending::None;
                self.reset_pending();
                return match ch_opt {
                    Some('h') => VimAction::FocusPaneLeft,
                    Some('l') => VimAction::FocusPaneRight,
                    Some('w') => VimAction::FocusOtherPane,
                    Some('v') => VimAction::SplitRight,
                    Some('x') | Some('r') => VimAction::SwapPanes,
                    Some('q') | Some('c') => VimAction::CloseSplit,
                    _ => VimAction::Consumed,
                };
            }
            VimPending::None => {}
        }

        // Ctrl combinations in Normal / Visual mode.
        if mods.ctrl {
            return match ch_opt {
                Some('d') => self.dispatch_motion(VimMotion::HalfPageDown),
                Some('u') => self.dispatch_motion(VimMotion::HalfPageUp),
                Some('r') => {
                    self.reset_pending();
                    VimAction::Redo
                }
                Some('w') => {
                    self.pending = VimPending::WindowPrefix;
                    VimAction::Consumed
                }
                _ => VimAction::Consumed,
            };
        }

        // Arrow keys also work naturally in Normal & Visual modes.
        match key {
            Key::Left => return self.dispatch_motion(VimMotion::Left),
            Key::Right => return self.dispatch_motion(VimMotion::Right),
            Key::Up => return self.dispatch_motion(VimMotion::Up),
            Key::Down => return self.dispatch_motion(VimMotion::Down),
            Key::Home => return self.dispatch_motion(VimMotion::LineStart),
            Key::End => return self.dispatch_motion(VimMotion::LineEnd),
            _ => {}
        }

        let Some(ch) = ch_opt else {
            return VimAction::Consumed;
        };

        // Numeric count prefix (`1`-`9`, or `0` when count already started).
        if ch.is_ascii_digit() && (ch != '0' || self.count > 0) {
            let d = (ch as u8 - b'0') as usize;
            self.count = self.count.saturating_mul(10).saturating_add(d).min(9999);
            return VimAction::Consumed;
        }

        // Inner text object prefix when an operator is pending (e.g. `ciw`, `di"`, `yi(`).
        if ch == 'i' && self.operator.is_some() {
            self.pending = VimPending::InnerObject;
            return VimAction::Consumed;
        }

        // Motions
        match ch {
            'h' => return self.dispatch_motion(VimMotion::Left),
            'j' => return self.dispatch_motion(VimMotion::Down),
            'k' => return self.dispatch_motion(VimMotion::Up),
            'l' => return self.dispatch_motion(VimMotion::Right),
            'w' | 'W' => return self.dispatch_motion(VimMotion::WordForward),
            'b' | 'B' => return self.dispatch_motion(VimMotion::WordBackward),
            'e' | 'E' => return self.dispatch_motion(VimMotion::WordEnd),
            '0' => return self.dispatch_motion(VimMotion::LineStart),
            '^' | '_' => return self.dispatch_motion(VimMotion::FirstNonBlank),
            '$' => return self.dispatch_motion(VimMotion::LineEnd),
            'G' => return self.dispatch_motion(VimMotion::FileBottom),
            '%' => return self.dispatch_motion(VimMotion::MatchingBracket),
            '{' => return self.dispatch_motion(VimMotion::ParagraphPrev),
            '}' => return self.dispatch_motion(VimMotion::ParagraphNext),
            'g' => {
                self.pending = VimPending::GPrefix;
                return VimAction::Consumed;
            }
            'z' => {
                self.pending = VimPending::ZPrefix;
                return VimAction::Consumed;
            }
            'f' => {
                self.pending = VimPending::FindChar(FindCharKind::Forward);
                return VimAction::Consumed;
            }
            'F' => {
                self.pending = VimPending::FindChar(FindCharKind::Backward);
                return VimAction::Consumed;
            }
            't' => {
                self.pending = VimPending::FindChar(FindCharKind::TillForward);
                return VimAction::Consumed;
            }
            'T' => {
                self.pending = VimPending::FindChar(FindCharKind::TillBackward);
                return VimAction::Consumed;
            }
            ';' => {
                if let Some((kind, c)) = self.last_find {
                    return self.dispatch_motion(VimMotion::FindChar { kind, ch: c });
                }
                return VimAction::Consumed;
            }
            ',' => {
                if let Some((kind, c)) = self.last_find {
                    let rev = match kind {
                        FindCharKind::Forward => FindCharKind::Backward,
                        FindCharKind::Backward => FindCharKind::Forward,
                        FindCharKind::TillForward => FindCharKind::TillBackward,
                        FindCharKind::TillBackward => FindCharKind::TillForward,
                    };
                    return self.dispatch_motion(VimMotion::FindChar { kind: rev, ch: c });
                }
                return VimAction::Consumed;
            }
            _ => {}
        }

        // Visual mode operator execution (`d`, `x`, `c`, `y`, `>`, `<`).
        if matches!(self.mode, VimMode::Visual | VimMode::VisualLine) {
            match ch {
                'v' => {
                    if self.mode == VimMode::Visual {
                        self.mode = VimMode::Normal;
                        return VimAction::ExitToNormal;
                    }
                    self.mode = VimMode::Visual;
                    return VimAction::Consumed;
                }
                'V' => {
                    if self.mode == VimMode::VisualLine {
                        self.mode = VimMode::Normal;
                        return VimAction::ExitToNormal;
                    }
                    self.mode = VimMode::VisualLine;
                    return VimAction::Consumed;
                }
                'd' | 'x' => {
                    self.mode = VimMode::Normal;
                    self.reset_pending();
                    return VimAction::VisualApply(VimOperator::Delete);
                }
                'c' | 's' => {
                    self.mode = VimMode::Insert;
                    self.reset_pending();
                    return VimAction::VisualApply(VimOperator::Change);
                }
                'y' => {
                    self.mode = VimMode::Normal;
                    self.reset_pending();
                    return VimAction::VisualApply(VimOperator::Yank);
                }
                '>' => {
                    self.mode = VimMode::Normal;
                    self.reset_pending();
                    return VimAction::VisualApply(VimOperator::Indent);
                }
                '<' => {
                    self.mode = VimMode::Normal;
                    self.reset_pending();
                    return VimAction::VisualApply(VimOperator::Outdent);
                }
                _ => return VimAction::Consumed,
            }
        }

        // Normal mode operators (`d`, `c`, `y`, `>`, `<` — doubled operates on lines!).
        let op_opt = match ch {
            'd' => Some(VimOperator::Delete),
            'c' => Some(VimOperator::Change),
            'y' => Some(VimOperator::Yank),
            '>' => Some(VimOperator::Indent),
            '<' => Some(VimOperator::Outdent),
            _ => None,
        };
        if let Some(op) = op_opt {
            if self.operator == Some(op) {
                self.operator = None;
                let count = self.take_count();
                if op == VimOperator::Change {
                    self.mode = VimMode::Insert;
                }
                return VimAction::OperatorLines { op, count };
            }
            self.operator = Some(op);
            return VimAction::Consumed;
        }

        // Normal mode single-key commands.
        self.operator = None;
        match ch {
            'i' => {
                self.reset_pending();
                self.mode = VimMode::Insert;
                VimAction::EnterInsert {
                    kind: InsertEntry::Before,
                }
            }
            'a' => {
                self.reset_pending();
                self.mode = VimMode::Insert;
                VimAction::EnterInsert {
                    kind: InsertEntry::After,
                }
            }
            'I' => {
                self.reset_pending();
                self.mode = VimMode::Insert;
                VimAction::EnterInsert {
                    kind: InsertEntry::LineStartNonBlank,
                }
            }
            'A' => {
                self.reset_pending();
                self.mode = VimMode::Insert;
                VimAction::EnterInsert {
                    kind: InsertEntry::LineEnd,
                }
            }
            'o' => {
                self.reset_pending();
                self.mode = VimMode::Insert;
                VimAction::EnterInsert {
                    kind: InsertEntry::OpenBelow,
                }
            }
            'O' => {
                self.reset_pending();
                self.mode = VimMode::Insert;
                VimAction::EnterInsert {
                    kind: InsertEntry::OpenAbove,
                }
            }
            's' => {
                self.reset_pending();
                self.mode = VimMode::Insert;
                VimAction::EnterInsert {
                    kind: InsertEntry::SubstituteChar,
                }
            }
            'S' => {
                let count = self.take_count();
                self.mode = VimMode::Insert;
                VimAction::OperatorLines {
                    op: VimOperator::Change,
                    count,
                }
            }
            'C' => {
                self.reset_pending();
                self.mode = VimMode::Insert;
                VimAction::EnterInsert {
                    kind: InsertEntry::ChangeToEnd,
                }
            }
            'D' => {
                self.reset_pending();
                VimAction::OperatorMotion {
                    op: VimOperator::Delete,
                    motion: VimMotion::LineEnd,
                    count: 1,
                }
            }
            'Y' => {
                let count = self.take_count();
                VimAction::OperatorLines {
                    op: VimOperator::Yank,
                    count,
                }
            }
            'x' => {
                let count = self.take_count();
                VimAction::DeleteChar {
                    before: false,
                    count,
                }
            }
            'X' => {
                let count = self.take_count();
                VimAction::DeleteChar {
                    before: true,
                    count,
                }
            }
            'r' => {
                self.pending = VimPending::ReplaceChar;
                VimAction::Consumed
            }
            'J' => {
                let count = self.take_count();
                VimAction::JoinLines(count)
            }
            'p' => {
                self.reset_pending();
                VimAction::Paste {
                    before: false,
                    linewise: self.last_yank_linewise,
                }
            }
            'P' => {
                self.reset_pending();
                VimAction::Paste {
                    before: true,
                    linewise: self.last_yank_linewise,
                }
            }
            'u' => {
                self.reset_pending();
                VimAction::Undo
            }
            'v' => {
                self.reset_pending();
                self.mode = VimMode::Visual;
                VimAction::Consumed
            }
            'V' => {
                self.reset_pending();
                self.mode = VimMode::VisualLine;
                VimAction::Consumed
            }
            '/' => {
                self.reset_pending();
                VimAction::OpenFind
            }
            'n' => {
                self.reset_pending();
                VimAction::SearchNext(true)
            }
            'N' => {
                self.reset_pending();
                VimAction::SearchNext(false)
            }
            '*' => {
                self.reset_pending();
                VimAction::SearchWordUnderCursor
            }
            _ => {
                self.reset_pending();
                VimAction::Consumed
            }
        }
    }
}
