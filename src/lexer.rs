//! 字句解析。ソースをトークンの列にする。
//!
//! 改行は空白と同じ扱い。コメントは整形ツールで使うので、捨てずに `comments` に残す。

use crate::source::{Diag, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kw {
    Function,
    Nothing,
    If,
    Else,
    While,
    Break,
    Continue,
    Return,
    Struct,
    Immutable,
    Import,
    Export,
    And,
    Nand,
    Or,
    Xor,
    Nor,
    Xnor,
    Not,
    Null,
    Maybe,
    True,
    False,
    Arena,
    To,
    Extern,
    As,
    // 型名
    Bool,
    Decnum,
    String,
    Pointer,
    Array,
    Address,
}

impl Kw {
    fn from_str(s: &str) -> Option<Kw> {
        Some(match s {
            "function" => Kw::Function,
            "nothing" => Kw::Nothing,
            "if" => Kw::If,
            "else" => Kw::Else,
            "while" => Kw::While,
            "break" => Kw::Break,
            "continue" => Kw::Continue,
            "return" => Kw::Return,
            "struct" => Kw::Struct,
            "immutable" => Kw::Immutable,
            "import" => Kw::Import,
            "export" => Kw::Export,
            "and" => Kw::And,
            "nand" => Kw::Nand,
            "or" => Kw::Or,
            "xor" => Kw::Xor,
            "nor" => Kw::Nor,
            "xnor" => Kw::Xnor,
            "not" => Kw::Not,
            "null" => Kw::Null,
            "maybe" => Kw::Maybe,
            "true" => Kw::True,
            "false" => Kw::False,
            "arena" => Kw::Arena,
            "to" => Kw::To,
            "extern" => Kw::Extern,
            "as" => Kw::As,
            "bool" => Kw::Bool,
            "decnum" => Kw::Decnum,
            "string" => Kw::String,
            "pointer" => Kw::Pointer,
            "array" => Kw::Array,
            "address" => Kw::Address,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kw::Function => "function",
            Kw::Nothing => "nothing",
            Kw::If => "if",
            Kw::Else => "else",
            Kw::While => "while",
            Kw::Break => "break",
            Kw::Continue => "continue",
            Kw::Return => "return",
            Kw::Struct => "struct",
            Kw::Immutable => "immutable",
            Kw::Import => "import",
            Kw::Export => "export",
            Kw::And => "and",
            Kw::Nand => "nand",
            Kw::Or => "or",
            Kw::Xor => "xor",
            Kw::Nor => "nor",
            Kw::Xnor => "xnor",
            Kw::Not => "not",
            Kw::Null => "null",
            Kw::Maybe => "maybe",
            Kw::True => "true",
            Kw::False => "false",
            Kw::Arena => "arena",
            Kw::To => "to",
            Kw::Extern => "extern",
            Kw::As => "as",
            Kw::Bool => "bool",
            Kw::Decnum => "decnum",
            Kw::String => "string",
            Kw::Pointer => "pointer",
            Kw::Array => "array",
            Kw::Address => "address",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    /// 整数。`8`・`32`・`64` は型名にもなるので、どちらかは構文解析で決める。
    Int(u64),
    Float(f64),
    Str(Vec<u8>),
    Char(u8),
    Kw(Kw),
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Semi,
    Colon,
    Assign,
    Dot,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Caret,
    Lt,
    Gt,
    Le,
    Ge,
    EqEq,
    Ne,
    Shl,
    Shr,
    Eof,
}

impl Tok {
    /// エラーメッセージ用の表記。
    pub fn describe(&self) -> String {
        match self {
            Tok::Ident(s) => format!("名前 `{}`", s),
            Tok::Int(n) => format!("数 `{}`", n),
            Tok::Float(f) => format!("小数 `{}`", f),
            Tok::Str(_) => "文字列".to_string(),
            Tok::Char(_) => "文字コード".to_string(),
            Tok::Kw(k) => format!("`{}`", k.as_str()),
            Tok::Eof => "ファイルの終わり".to_string(),
            t => format!("`{}`", t.punct_str()),
        }
    }

    pub fn punct_str(&self) -> &'static str {
        match self {
            Tok::LParen => "(",
            Tok::RParen => ")",
            Tok::LBrace => "{",
            Tok::RBrace => "}",
            Tok::LBracket => "[",
            Tok::RBracket => "]",
            Tok::Comma => ",",
            Tok::Semi => ";",
            Tok::Colon => ":",
            Tok::Assign => "=",
            Tok::Dot => ".",
            Tok::Plus => "+",
            Tok::Minus => "-",
            Tok::Star => "*",
            Tok::Slash => "/",
            Tok::Percent => "%",
            Tok::Caret => "^",
            Tok::Lt => "<",
            Tok::Gt => ">",
            Tok::Le => "<=",
            Tok::Ge => ">=",
            Tok::EqEq => "==",
            Tok::Ne => "!=",
            Tok::Shl => "<<",
            Tok::Shr => ">>",
            _ => "?",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

/// コメント。整形ツール（手順 8）で使う。
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct Comment {
    pub span: Span,
    /// `//` や `/* */` を含む元の文字列。
    pub text: String,
}

#[derive(Debug)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    #[allow(dead_code)]
    pub comments: Vec<Comment>,
}

/// `file` は何番目のファイルか（エラーの場所に使う）。
pub fn lex(src: &str, file: u32) -> Result<Lexed, Diag> {
    let mut lexed = Lexer {
        src,
        bytes: src.as_bytes(),
        pos: 0,
        tokens: Vec::new(),
        comments: Vec::new(),
    }
    .run()
    .map_err(|mut d| {
        d.span.file = file;
        d
    })?;
    for t in &mut lexed.tokens {
        t.span.file = file;
    }
    for c in &mut lexed.comments {
        c.span.file = file;
    }
    Ok(lexed)
}

struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    tokens: Vec<Token>,
    comments: Vec<Comment>,
}

impl<'a> Lexer<'a> {
    fn peek(&self) -> u8 {
        self.bytes.get(self.pos).copied().unwrap_or(0)
    }

    fn peek_at(&self, n: usize) -> u8 {
        self.bytes.get(self.pos + n).copied().unwrap_or(0)
    }

    fn push(&mut self, tok: Tok, lo: usize) {
        self.tokens.push(Token {
            tok,
            span: Span::new(lo, self.pos),
        });
    }

    fn run(mut self) -> Result<Lexed, Diag> {
        loop {
            self.skip_space_and_comments()?;
            let lo = self.pos;
            if self.pos >= self.bytes.len() {
                self.push(Tok::Eof, lo);
                break;
            }
            let c = self.peek();
            if c.is_ascii_alphabetic() || c == b'_' {
                while self.peek().is_ascii_alphanumeric() || self.peek() == b'_' {
                    self.pos += 1;
                }
                let word = &self.src[lo..self.pos];
                let tok = match Kw::from_str(word) {
                    Some(k) => Tok::Kw(k),
                    None => Tok::Ident(word.to_string()),
                };
                self.push(tok, lo);
            } else if c.is_ascii_digit() {
                self.number(lo)?;
            } else if c == b'"' {
                self.string(lo)?;
            } else if c == b'\'' {
                self.char_lit(lo)?;
            } else if c < 0x80 {
                self.punct(lo)?;
            } else {
                return Err(self.non_ascii(lo));
            }
        }
        Ok(Lexed {
            tokens: self.tokens,
            comments: self.comments,
        })
    }

    fn skip_space_and_comments(&mut self) -> Result<(), Diag> {
        loop {
            let c = self.peek();
            if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' {
                self.pos += 1;
            } else if c == b'/' && self.peek_at(1) == b'/' {
                let lo = self.pos;
                while self.pos < self.bytes.len() && self.peek() != b'\n' {
                    self.pos += 1;
                }
                self.comments.push(Comment {
                    span: Span::new(lo, self.pos),
                    text: self.src[lo..self.pos].trim_end_matches('\r').to_string(),
                });
            } else if c == b'/' && self.peek_at(1) == b'*' {
                let lo = self.pos;
                self.pos += 2;
                loop {
                    if self.pos >= self.bytes.len() {
                        return Err(Diag::new(Span::new(lo, lo + 2), "`/*` が閉じていません")
                            .hint("コメントの終わりに `*/` を書いてください"));
                    }
                    if self.peek() == b'*' && self.peek_at(1) == b'/' {
                        self.pos += 2;
                        break;
                    }
                    self.pos += 1;
                }
                self.comments.push(Comment {
                    span: Span::new(lo, self.pos),
                    text: self.src[lo..self.pos].to_string(),
                });
            } else {
                return Ok(());
            }
        }
    }

    fn number(&mut self, lo: usize) -> Result<(), Diag> {
        if self.peek() == b'0' && (self.peek_at(1) == b'x' || self.peek_at(1) == b'X') {
            self.pos += 2;
            let start = self.pos;
            while self.peek().is_ascii_hexdigit() {
                self.pos += 1;
            }
            if self.pos == start {
                return Err(Diag::new(
                    Span::new(lo, self.pos),
                    "`0x` の後に 16 進数がありません",
                ));
            }
            if self.peek().is_ascii_alphanumeric() || self.peek() == b'_' {
                return Err(self.bad_number(lo));
            }
            let v = u64::from_str_radix(&self.src[start..self.pos], 16)
                .map_err(|_| Diag::new(Span::new(lo, self.pos), "数が大きすぎます（64bit に入りません）"))?;
            self.push(Tok::Int(v), lo);
            return Ok(());
        }
        while self.peek().is_ascii_digit() {
            self.pos += 1;
        }
        let mut is_float = false;
        // 小数。`3.14` のように `.` の後に数字があるときだけ（`a.b` のフィールドと区別する）。
        if self.peek() == b'.' && self.peek_at(1).is_ascii_digit() {
            self.pos += 1;
            while self.peek().is_ascii_digit() {
                self.pos += 1;
            }
            is_float = true;
        }
        // 指数。`1e10`、`2.5e-3`
        let sign = matches!(self.peek_at(1), b'+' | b'-');
        if matches!(self.peek(), b'e' | b'E')
            && (self.peek_at(1).is_ascii_digit() || (sign && self.peek_at(2).is_ascii_digit()))
        {
            self.pos += if sign { 2 } else { 1 };
            while self.peek().is_ascii_digit() {
                self.pos += 1;
            }
            is_float = true;
        }
        if is_float {
            if self.peek().is_ascii_alphanumeric() || self.peek() == b'_' {
                return Err(self.bad_number(lo));
            }
            let v: f64 = self.src[lo..self.pos].parse().expect("小数の形は確認済み");
            self.push(Tok::Float(v), lo);
            return Ok(());
        }
        if self.peek().is_ascii_alphabetic() || self.peek() == b'_' {
            return Err(self.bad_number(lo));
        }
        let v: u64 = self.src[lo..self.pos]
            .parse()
            .map_err(|_| Diag::new(Span::new(lo, self.pos), "数が大きすぎます（64bit に入りません）"))?;
        self.push(Tok::Int(v), lo);
        Ok(())
    }

    fn bad_number(&mut self, lo: usize) -> Diag {
        while self.peek().is_ascii_alphanumeric() || self.peek() == b'_' {
            self.pos += 1;
        }
        Diag::new(
            Span::new(lo, self.pos),
            format!("`{}` は数として読めません", &self.src[lo..self.pos]),
        )
        .hint("名前は数字で始められません")
    }

    /// エスケープを 1 つ読む（`\` の次から）。読んだバイトを返す。
    fn escape(&mut self, backslash: usize) -> Result<u8, Diag> {
        let rest = &self.src[self.pos..];
        let (len, byte) = if rest.starts_with("lb") {
            (2, b'\n')
        } else if rest.starts_with("tab") {
            (3, b'\t')
        } else {
            match self.peek() {
                b'n' => (1, b'\n'),
                b't' => (1, b'\t'),
                b'r' => (1, b'\r'),
                b'"' => (1, b'"'),
                b'\'' => (1, b'\''),
                b'\\' => (1, b'\\'),
                b'0' => (1, 0),
                _ => {
                    let end = self.src[self.pos..]
                        .chars()
                        .next()
                        .map(|c| self.pos + c.len_utf8())
                        .unwrap_or(self.pos);
                    return Err(Diag::new(
                        Span::new(backslash, end),
                        format!("`{}` というエスケープはありません", &self.src[backslash..end]),
                    )
                    .hint("使えるのは \\lb \\n \\tab \\t \\r \\\" \\' \\\\ \\0 です。\\ そのものは \\\\ と書きます"));
                }
            }
        };
        self.pos += len;
        Ok(byte)
    }

    fn string(&mut self, lo: usize) -> Result<(), Diag> {
        self.pos += 1;
        let mut buf = Vec::new();
        loop {
            if self.pos >= self.bytes.len() {
                return Err(Diag::new(Span::new(lo, lo + 1), "文字列が閉じていません")
                    .hint("文字列の終わりに `\"` を書いてください"));
            }
            match self.peek() {
                b'"' => {
                    self.pos += 1;
                    break;
                }
                b'\\' => {
                    let bs = self.pos;
                    self.pos += 1;
                    buf.push(self.escape(bs)?);
                }
                b => {
                    // 途中の改行もそのまま文字列に入れる。
                    buf.push(b);
                    self.pos += 1;
                }
            }
        }
        self.push(Tok::Str(buf), lo);
        Ok(())
    }

    fn char_lit(&mut self, lo: usize) -> Result<(), Diag> {
        self.pos += 1;
        let byte = match self.peek() {
            b'\'' => {
                self.pos += 1;
                return Err(Diag::new(Span::new(lo, self.pos), "`''` の中に文字がありません"));
            }
            b'\\' => {
                let bs = self.pos;
                self.pos += 1;
                self.escape(bs)?
            }
            b if b >= 0x80 => {
                let c = self.src[self.pos..].chars().next().unwrap();
                self.pos += c.len_utf8();
                if self.peek() == b'\'' {
                    self.pos += 1;
                }
                return Err(Diag::new(
                    Span::new(lo, self.pos),
                    format!("`{}` は 1 バイトに収まらないので、`'` で書けません", c),
                )
                .hint("1 文字の文字列として \"…\" で書いてください"));
            }
            0 | b'\n' => {
                return Err(Diag::new(Span::new(lo, lo + 1), "`'` が閉じていません"));
            }
            b => {
                self.pos += 1;
                b
            }
        };
        if self.peek() != b'\'' {
            return Err(
                Diag::new(Span::new(lo, self.pos), "`'` の中に書けるのは 1 文字だけです")
                    .hint("2 文字以上なら文字列として \"…\" で書いてください"),
            );
        }
        self.pos += 1;
        self.push(Tok::Char(byte), lo);
        Ok(())
    }

    fn punct(&mut self, lo: usize) -> Result<(), Diag> {
        let c = self.peek();
        let n = self.peek_at(1);
        let (tok, len) = match (c, n) {
            (b'<', b'=') => (Tok::Le, 2),
            (b'>', b'=') => (Tok::Ge, 2),
            (b'=', b'=') => (Tok::EqEq, 2),
            (b'!', b'=') => (Tok::Ne, 2),
            (b'<', b'<') => (Tok::Shl, 2),
            (b'>', b'>') => (Tok::Shr, 2),
            (b'(', _) => (Tok::LParen, 1),
            (b')', _) => (Tok::RParen, 1),
            (b'{', _) => (Tok::LBrace, 1),
            (b'}', _) => (Tok::RBrace, 1),
            (b'[', _) => (Tok::LBracket, 1),
            (b']', _) => (Tok::RBracket, 1),
            (b',', _) => (Tok::Comma, 1),
            (b';', _) => (Tok::Semi, 1),
            (b':', _) => (Tok::Colon, 1),
            (b'=', _) => (Tok::Assign, 1),
            (b'.', _) => (Tok::Dot, 1),
            (b'+', _) => (Tok::Plus, 1),
            (b'-', _) => (Tok::Minus, 1),
            (b'*', _) => (Tok::Star, 1),
            (b'/', _) => (Tok::Slash, 1),
            (b'%', _) => (Tok::Percent, 1),
            (b'^', _) => (Tok::Caret, 1),
            (b'<', _) => (Tok::Lt, 1),
            (b'>', _) => (Tok::Gt, 1),
            _ => return Err(self.unknown_punct(lo)),
        };
        self.pos += len;
        self.push(tok, lo);
        Ok(())
    }

    /// 使えない記号。ほかの言語から来た人がよく書くものにはヒントを付ける。
    fn unknown_punct(&self, lo: usize) -> Diag {
        let two = &self.src[lo..(lo + 2).min(self.src.len())];
        let (len, hint) = match two {
            "&&" => (2, Some("「かつ」は `and` と書きます")),
            "||" => (2, Some("「または」は `or` と書きます")),
            _ => match self.peek() {
                b'&' => (1, Some("ビットごとの AND は `and` と書きます")),
                b'|' => (1, Some("ビットごとの OR は `or` と書きます")),
                b'!' => (1, Some("否定は `not` と書きます")),
                b'~' => (1, Some("ビットの反転は `not` と書きます")),
                b'#' => (1, Some("コメントは `//` か `/* */` で書きます")),
                _ => (1, None),
            },
        };
        let d = Diag::new(
            Span::new(lo, lo + len),
            format!("`{}` は orz では使えません", &self.src[lo..lo + len]),
        );
        match hint {
            Some(h) => d.hint(h),
            None => d,
        }
    }

    /// 文字列の外に ASCII 以外の文字があった。
    fn non_ascii(&self, lo: usize) -> Diag {
        let c = self.src[lo..].chars().next().unwrap();
        let span = Span::new(lo, lo + c.len_utf8());
        match c {
            '“' | '”' | '„' | '″' => Diag::new(span, format!("`{}` は全角の引用符です", c))
                .hint("半角の `\"` に直してください（スマートフォンでは自動で変わることがあります）"),
            '‘' | '’' | '′' => Diag::new(span, format!("`{}` は全角の引用符です", c))
                .hint("半角の `'` に直してください（スマートフォンでは自動で変わることがあります）"),
            '\u{3000}' => Diag::new(span, "全角スペースがあります").hint("半角スペースに直してください"),
            '；' | '：' | '（' | '）' | '｛' | '｝' | '＝' | '，' | '＋' | '－' | '＊' | '／' => {
                Diag::new(span, format!("`{}` は全角の記号です", c)).hint("半角に直してください")
            }
            _ => Diag::new(span, format!("`{}` は文字列やコメントの外では使えません", c))
                .hint("名前に使えるのは半角の英字・数字・`_` です"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(s: &str) -> Result<Lexed, Diag> {
        super::lex(s, 0)
    }

    fn toks(s: &str) -> Vec<Tok> {
        lex(s).unwrap().tokens.into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn basic() {
        assert_eq!(
            toks("x:32 = 0xFF + 'A';"),
            vec![
                Tok::Ident("x".into()),
                Tok::Colon,
                Tok::Int(32),
                Tok::Assign,
                Tok::Int(255),
                Tok::Plus,
                Tok::Char(65),
                Tok::Semi,
                Tok::Eof
            ]
        );
    }

    #[test]
    fn float_and_field() {
        assert_eq!(toks("2.5"), vec![Tok::Float(2.5), Tok::Eof]);
        assert_eq!(
            toks("1e3 2.5e-1 7E+2"),
            vec![Tok::Float(1000.0), Tok::Float(0.25), Tok::Float(700.0), Tok::Eof]
        );
        assert!(lex("1e").is_err());
        assert_eq!(
            toks("p.x"),
            vec![Tok::Ident("p".into()), Tok::Dot, Tok::Ident("x".into()), Tok::Eof]
        );
    }

    #[test]
    fn escapes() {
        assert_eq!(
            toks(r#""a\lb\n\tab\t\r\"\\\'\0""#),
            vec![Tok::Str(b"a\n\n\t\t\r\"\\'\0".to_vec()), Tok::Eof]
        );
        assert!(lex(r#""\q""#).is_err());
        assert_eq!(toks("\"a\nb\""), vec![Tok::Str(b"a\nb".to_vec()), Tok::Eof]);
    }

    #[test]
    fn comments_kept() {
        let l = lex("// a\nx /* b */ y").unwrap();
        assert_eq!(l.comments.len(), 2);
        assert_eq!(l.comments[0].text, "// a");
        assert_eq!(l.comments[1].text, "/* b */");
    }

    #[test]
    fn errors() {
        assert!(lex("“aaa”").unwrap_err().msg.contains("全角の引用符"));
        assert!(lex("'あ'").unwrap_err().msg.contains("1 バイト"));
        assert!(lex("a && b").unwrap_err().hint.unwrap().contains("and"));
        assert!(lex("12ab").is_err());
        assert!(lex("/* x").is_err());
    }
}
