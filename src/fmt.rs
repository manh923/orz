//! 整形ツール（`orz fmt`）。
//!
//! トークンを並べ直して書き出す。改行と字下げは `;`、`{`、`}` で決め、コメントは残す。
//! - 字下げは 4 つの空白。`} else {` は 1 行にまとめる
//! - 文と文の間の空行は 1 行まで残す（`{` の直後と `}` の直前の空行は消す）
//! - 文の途中の改行は詰める。行末のコメント（`//`）があれば、そこで改行して、続きを 1 段深く字下げする
//! - 行末のコメントの前の空白は、元のまま（そろえて書いたコメントがずれないように）
//! - トークンの間の空白：`a + b`、`f(x, y)`、`x:32`、`p.x`、`a[i]`、`-x`、`if (…)`

use crate::lexer::{self, Kw, Kw as K, Tok};
use crate::parser;
use crate::source::Diag;

const INDENT: &str = "    ";

/// ソースを整形する。字句や構文の誤りがあれば、そのエラーを返す（書き換えない）。
pub fn format(src: &str) -> Result<String, Diag> {
    let lexed = lexer::lex(src, 0)?;
    parser::parse(lexed.tokens.clone())?;
    // トークンとコメントを、ソースの順に並べる。
    let mut items: Vec<Item> = Vec::new();
    for t in &lexed.tokens {
        if t.tok != Tok::Eof {
            items.push(Item {
                lo: t.span.lo,
                hi: t.span.hi,
                tok: Some(t.tok.clone()),
            });
        }
    }
    for c in &lexed.comments {
        items.push(Item {
            lo: c.span.lo,
            hi: c.span.hi,
            tok: None,
        });
    }
    items.sort_by_key(|i| i.lo);

    let mut f = Fmt {
        out: String::new(),
        depth: 0,
        in_line: false,
        need_newline: false,
        cont: false,
        prev: None,
        prev_unary: false,
        space_next: false,
        after_open: true,
        close_brace_last: false,
        comment_line: false,
    };
    let mut end = 0;
    for item in &items {
        let gap = &src[end..item.lo];
        let newlines = gap.matches('\n').count();
        let text = src[item.lo..item.hi].replace('\r', "");
        match &item.tok {
            Some(tok) => f.token(tok, &text, newlines),
            None => f.comment(&text, gap, newlines),
        }
        end = item.hi;
    }
    if f.in_line {
        f.out.push('\n');
    }
    Ok(f.out)
}

struct Item {
    lo: usize,
    hi: usize,
    /// コメントなら `None`。
    tok: Option<Tok>,
}

struct Fmt {
    out: String,
    depth: usize,
    /// 今の行に何か書いたか。
    in_line: bool,
    /// 今の行は終わっている（次に書くものは、新しい行から）。
    need_newline: bool,
    /// 文の途中で改行した（続きを 1 段深く字下げする）。
    cont: bool,
    prev: Option<Tok>,
    /// 直前のトークンが、符号の `-` か。
    prev_unary: bool,
    /// 直前に書いたのが、文の途中のコメント（次のトークンの前に空白を入れる）。
    space_next: bool,
    /// 直前に書いたのが `{`（またはファイルの先頭）。この直後の空行は書かない。
    after_open: bool,
    /// 今の行が `}` で終わっている（`} else` をつなげる）。
    close_brace_last: bool,
    /// 今の行は、行の頭から書いた `/* */` のコメントだけ（元のソースで改行していれば、次は新しい行）。
    comment_line: bool,
}

impl Fmt {
    /// 新しい行を始める。元のソースで空行があれば（`newlines` が 2 以上）、空行を 1 つ入れる。
    fn start_line(&mut self, newlines: usize, closing: bool) {
        if self.in_line {
            self.out.push('\n');
            self.in_line = false;
        }
        self.need_newline = false;
        if newlines >= 2 && !self.after_open && !closing {
            self.out.push('\n');
        }
        let depth = self.depth + usize::from(self.cont);
        for _ in 0..depth {
            self.out.push_str(INDENT);
        }
        self.in_line = true;
        self.after_open = false;
        self.close_brace_last = false;
        self.comment_line = false;
    }

    /// 文の終わり（`;`、`{`、`}`）。次は新しい行から。
    fn end_statement(&mut self) {
        self.need_newline = true;
        self.cont = false;
    }

    fn token(&mut self, tok: &Tok, text: &str, newlines: usize) {
        if *tok == Tok::Kw(Kw::Else) && self.close_brace_last {
            // `} else`
            self.need_newline = false;
            self.out.push(' ');
        } else if *tok == Tok::RBrace {
            self.depth = self.depth.saturating_sub(1);
            self.cont = false;
            self.start_line(newlines, true);
        } else if self.need_newline || !self.in_line || (self.comment_line && newlines > 0) {
            if *tok == Tok::LBrace {
                // 行の頭に来た `{` は、文の続きとして字下げしない。
                self.cont = false;
            }
            self.start_line(newlines, false);
        } else if self.space_next || needs_space(self.prev.as_ref(), self.prev_unary, tok) {
            self.out.push(' ');
        }
        self.out.push_str(text);
        self.space_next = false;
        self.close_brace_last = false;
        self.comment_line = false;
        self.prev_unary = *tok == Tok::Minus && is_unary_position(self.prev.as_ref());
        self.prev = Some(tok.clone());
        match tok {
            Tok::LBrace => {
                self.depth += 1;
                self.end_statement();
                self.after_open = true;
            }
            Tok::RBrace => {
                self.end_statement();
                self.close_brace_last = true;
            }
            Tok::Semi => self.end_statement(),
            _ => {}
        }
    }

    fn comment(&mut self, text: &str, gap: &str, newlines: usize) {
        let line_comment = text.starts_with("//");
        // 元のソースで、前のトークンと同じ行にあるか。
        let trailing = newlines == 0 && self.in_line;
        // 文の途中か（今の行に、書きかけの文がある）。
        let mid = self.in_line && !self.need_newline && !self.comment_line;
        if trailing {
            if self.need_newline {
                // 文の後ろのコメント。前の空白は元のまま。
                let spaces = if gap.is_empty() || gap.contains('\t') {
                    1
                } else {
                    gap.chars().count()
                };
                self.out.push_str(&" ".repeat(spaces));
            } else {
                self.out.push(' ');
            }
        } else {
            if mid {
                self.cont = true;
            }
            self.start_line(newlines, false);
        }
        self.out.push_str(text);
        self.close_brace_last = false;
        if line_comment {
            if trailing && mid {
                // 文の途中で改行するので、続きは 1 段深く字下げする。
                self.cont = true;
            }
            self.need_newline = true;
        } else {
            self.space_next = true;
            if !trailing {
                self.comment_line = true;
            }
        }
    }
}

/// `-` が符号（`-x`）になる場所か。
fn is_unary_position(prev: Option<&Tok>) -> bool {
    use Tok::*;
    match prev {
        None => true,
        Some(t) => matches!(
            t,
            LParen
                | LBracket
                | Comma
                | Assign
                | Plus
                | Minus
                | Star
                | Slash
                | Percent
                | Caret
                | Lt
                | Gt
                | Le
                | Ge
                | EqEq
                | Ne
                | Shl
                | Shr
                | Kw(K::Return | K::And | K::Or | K::Xor | K::Nand | K::Nor | K::Xnor | K::Not)
        ),
    }
}

/// 2 つのトークンの間に空白を入れるか。
fn needs_space(prev: Option<&Tok>, prev_unary: bool, next: &Tok) -> bool {
    use Tok::*;
    let Some(prev) = prev else { return false };
    if prev_unary {
        return false;
    }
    let tight_before = |p: &Tok| matches!(p, LParen | LBracket | Colon | Dot);
    match next {
        RParen | RBracket | Comma | Semi | Dot | Colon => false,
        // 呼び出し・変換・関数の型（`f(x)`、`to:32(x)`、`function:32(32)`）は詰める。`if (`、`not (` は空ける。
        LParen => {
            !(tight_before(prev)
                || matches!(
                    prev,
                    Ident(_)
                        | Int(_)
                        | RParen
                        | RBracket
                        | Kw(K::Bool
                            | K::Decnum
                            | K::String
                            | K::Pointer
                            | K::Array
                            | K::Address
                            | K::Nothing)
                ))
        }
        // 添字（`a[i]`）は詰める。配列（`= [1, 2]`）は空ける。
        LBracket => !(tight_before(prev) || matches!(prev, Ident(_) | RParen | RBracket | Str(_))),
        _ => !tight_before(prev),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// コメントを除いたトークン（種類と、元の文字列）と、コメントの並び。
    fn shape(src: &str) -> (Vec<(String, String)>, Vec<String>) {
        let lexed = lexer::lex(src, 0).unwrap();
        let toks = lexed
            .tokens
            .iter()
            .map(|t| (format!("{:?}", t.tok), src[t.span.lo..t.span.hi].to_string()))
            .collect();
        let comments = lexed
            .comments
            .iter()
            .map(|c| src[c.span.lo..c.span.hi].replace('\r', ""))
            .collect();
        (toks, comments)
    }

    /// テストのファイルをすべて整形して、トークンとコメントが変わらないこと、2 回目で変わらないことを確かめる。
    #[test]
    fn all_cases() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cases");
        let mut files = Vec::new();
        for dir in [root.clone(), root.join("modules")] {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.extension().is_some_and(|e| e == "orz") {
                    files.push(p);
                }
            }
        }
        let mut formatted = 0;
        for p in &files {
            let src = std::fs::read_to_string(p).unwrap();
            // 字句や構文の誤りを試すファイルは、整形できない。
            let Ok(once) = format(&src) else { continue };
            formatted += 1;
            assert_eq!(
                shape(&src),
                shape(&once),
                "{} でトークンかコメントが変わった",
                p.display()
            );
            let twice = format(&once).unwrap();
            assert_eq!(once, twice, "{} で 2 回目の整形の結果が変わった", p.display());
            assert!(
                !once.lines().any(|l| l.ends_with(' ')),
                "{} で行の終わりに空白がある",
                p.display()
            );
        }
        assert!(
            formatted > 150,
            "整形できたファイルが少なすぎる（{} 個）",
            formatted
        );
    }

    #[test]
    fn layout() {
        let src = "// 先頭\nimport stdio;\nstruct P{x:32;y:32;}\n\n\nfunction 32 f(a:32,b:32){\n\nreturn -a+b;\n\n}\n\
                   function nothing main( ){\np:P=P(1,-2);   // 点\nif(p.x>0){stdio.print(p.x);}\nelse{\n\
                   /* 説明 */\nx:32 = f(1, // 1 つ目\n2);\n}\na:array:32=[1,2];stdio.print(a[0]);\n}\n";
        let want =
            "// 先頭\nimport stdio;\nstruct P {\n    x:32;\n    y:32;\n}\n\nfunction 32 f(a:32, b:32) {\n\
                    \x20   return -a + b;\n}\nfunction nothing main() {\n    p:P = P(1, -2);   // 点\n\
                    \x20   if (p.x > 0) {\n        stdio.print(p.x);\n    } else {\n        /* 説明 */\n\
                    \x20       x:32 = f(1, // 1 つ目\n            2);\n    }\n    a:array:32 = [1, 2];\n\
                    \x20   stdio.print(a[0]);\n}\n";
        assert_eq!(format(src).unwrap(), want);
    }
}
