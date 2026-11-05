//! ソースファイルと、エラーの位置・表示。

use std::fmt::Write as _;

/// ソースの中の範囲（バイト位置、`lo..hi`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Span {
    pub lo: usize,
    pub hi: usize,
}

impl Span {
    pub fn new(lo: usize, hi: usize) -> Span {
        Span { lo, hi }
    }

    /// 2 つの範囲をまとめた範囲。
    pub fn to(self, other: Span) -> Span {
        Span::new(self.lo.min(other.lo), self.hi.max(other.hi))
    }
}

pub struct SourceFile {
    pub name: String,
    pub text: String,
    line_starts: Vec<usize>,
}

impl SourceFile {
    pub fn new(name: impl Into<String>, text: impl Into<String>) -> SourceFile {
        let text = text.into();
        let mut line_starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        SourceFile {
            name: name.into(),
            text,
            line_starts,
        }
    }

    /// バイト位置 → 行番号（1 から）。
    pub fn line_of(&self, pos: usize) -> usize {
        match self.line_starts.binary_search(&pos) {
            Ok(i) => i + 1,
            Err(i) => i,
        }
    }

    /// バイト位置 → （行、列）。列は 1 からの文字数。
    pub fn line_col(&self, pos: usize) -> (usize, usize) {
        let line = self.line_of(pos);
        let start = self.line_starts[line - 1];
        let col = self.text[start..pos.min(self.text.len())].chars().count() + 1;
        (line, col)
    }

    /// 行の中身（改行を除く）。
    pub fn line_text(&self, line: usize) -> &str {
        let start = self.line_starts[line - 1];
        let end = self
            .line_starts
            .get(line)
            .map(|e| e - 1)
            .unwrap_or(self.text.len());
        self.text[start..end.max(start)].trim_end_matches('\r')
    }
}

/// コンパイルエラー。
#[derive(Debug, Clone)]
pub struct Diag {
    pub span: Span,
    pub msg: String,
    pub hint: Option<String>,
}

impl Diag {
    pub fn new(span: Span, msg: impl Into<String>) -> Diag {
        Diag {
            span,
            msg: msg.into(),
            hint: None,
        }
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Diag {
        self.hint = Some(hint.into());
        self
    }

    /// エラーを、前の行と印つきで表示用の文字列にする。
    /// `;` を忘れると次の行でエラーになるので、前の行も必ず見せる。
    pub fn render(&self, src: &SourceFile) -> String {
        let (line, col) = src.line_col(self.span.lo);
        let mut out = String::new();
        let _ = writeln!(out, "{}:{}:{}: エラー: {}", src.name, line, col, self.msg);
        let width = line.to_string().len();
        if line > 1 {
            let _ = writeln!(out, " {:>width$} | {}", line - 1, src.line_text(line - 1));
        }
        let text = src.line_text(line);
        let _ = writeln!(out, " {:>width$} | {}", line, text);
        // 印の位置。全角文字は 2 文字分の幅として数える。
        let start = src.line_starts[line - 1];
        let before = &src.text[start..self.span.lo.min(src.text.len())];
        let pad: usize = before.chars().map(display_width).sum();
        let hi = self.span.hi.min(start + text.len()).max(self.span.lo);
        let marked = &src.text[self.span.lo.min(src.text.len())..hi];
        let len: usize = marked.chars().map(display_width).sum::<usize>().max(1);
        let _ = writeln!(out, " {:>width$} | {}{}", "", " ".repeat(pad), "^".repeat(len));
        if let Some(h) = &self.hint {
            let _ = writeln!(out, " {:>width$} = ヒント: {}", "", h);
        }
        out
    }
}

/// 端末での文字の幅（おおざっぱ）。
fn display_width(c: char) -> usize {
    match c as u32 {
        0x1100..=0x115F
        | 0x2E80..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1FAFF
        | 0x20000..=0x3FFFD => 2,
        _ => 1,
    }
}
