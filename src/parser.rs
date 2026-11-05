//! 構文解析。トークンの列を構文木にする。

use crate::ast::*;
use crate::lexer::{Kw, Tok, Token};
use crate::source::{Diag, Span};

pub fn parse(tokens: Vec<Token>) -> Result<Program, Diag> {
    let mut p = Parser { tokens, pos: 0 };
    let mut items = Vec::new();
    while p.peek() != &Tok::Eof {
        items.push(p.item()?);
    }
    Ok(Program { items })
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

type PResult<T> = Result<T, Diag>;

impl Parser {
    fn peek(&self) -> &Tok {
        &self.tokens[self.pos].tok
    }

    fn peek_at(&self, n: usize) -> &Tok {
        let i = (self.pos + n).min(self.tokens.len() - 1);
        &self.tokens[i].tok
    }

    fn span(&self) -> Span {
        self.tokens[self.pos].span
    }

    fn prev_span(&self) -> Span {
        self.tokens[self.pos.saturating_sub(1)].span
    }

    fn bump(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn eat(&mut self, tok: &Tok) -> bool {
        if self.peek() == tok {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, tok: Tok, what: &str) -> PResult<Span> {
        if self.peek() == &tok {
            Ok(self.bump().span)
        } else {
            Err(self.unexpected(&format!("{} `{}`", what, tok.punct_str())))
        }
    }

    /// 期待と違うトークンがあった。
    fn unexpected(&self, expected: &str) -> Diag {
        let d = Diag::new(
            self.span(),
            format!("{}が必要ですが、{}があります", expected, self.peek().describe()),
        );
        // `;` の書き忘れは、次の行の頭でエラーになることが多い。
        if expected.contains('`') && expected.contains(';') {
            d.hint("前の行の終わりに `;` を書き忘れていませんか")
        } else {
            d
        }
    }

    fn ident(&mut self, what: &str) -> PResult<(String, Span)> {
        match self.peek().clone() {
            Tok::Ident(name) => Ok((name, self.bump().span)),
            Tok::Kw(k) => Err(Diag::new(
                self.span(),
                format!("`{}` はキーワードなので、{}には使えません", k.as_str(), what),
            )),
            _ => Err(self.unexpected(what)),
        }
    }

    fn not_yet(&self, what: &str) -> Diag {
        Diag::new(self.span(), format!("{}はまだ実装していません", what))
    }

    // ---------------- トップレベル ----------------

    fn item(&mut self) -> PResult<Item> {
        match self.peek() {
            Tok::Kw(Kw::Function) => Ok(Item::Func(self.func()?)),
            Tok::Ident(_) => Ok(Item::Global(self.decl()?)),
            Tok::Kw(Kw::Struct) => Err(self.not_yet("`struct`")),
            Tok::Kw(Kw::Import) => Err(self.not_yet("`import`")),
            Tok::Kw(Kw::Export) => Err(self.not_yet("`export`")),
            Tok::Kw(Kw::Extern) => Err(self.not_yet("`extern`")),
            Tok::Kw(Kw::Immutable) => Err(self.not_yet("`immutable`")),
            _ => Err(self.unexpected("関数（`function`）かグローバル変数の宣言")),
        }
    }

    fn func(&mut self) -> PResult<Func> {
        self.bump(); // function
        if self.peek() == &Tok::Colon {
            return Err(
                Diag::new(self.span(), "`function` の直後の戻り値の型には `:` を付けません")
                    .hint("例：function 32 add(a:32, b:32) { ... }"),
            );
        }
        let ret = self.ret_type()?;
        let (name, name_span) = self.ident("関数の名前")?;
        if self.peek() == &Tok::Colon {
            return Err(self.not_yet("ジェネリクス"));
        }
        self.expect(Tok::LParen, "引数の始まりの")?;
        let mut params = Vec::new();
        if !self.eat(&Tok::RParen) {
            loop {
                let (pname, pspan) = self.ident("引数の名前")?;
                if self.peek() != &Tok::Colon {
                    return Err(
                        Diag::new(self.span(), format!("引数 `{}` の型がありません", pname))
                            .hint(format!("`{}:32` のように `:` と型を書いてください", pname)),
                    );
                }
                self.bump();
                let ty = self.type_expr()?;
                params.push(Param {
                    name: pname,
                    span: pspan.to(ty.span),
                    ty,
                });
                if self.eat(&Tok::Comma) {
                    continue;
                }
                self.expect(Tok::RParen, "引数の終わりの")?;
                break;
            }
        }
        let body = self.block()?;
        Ok(Func {
            name,
            name_span,
            ret,
            params,
            body,
        })
    }

    fn ret_type(&mut self) -> PResult<RetType> {
        if self.peek() == &Tok::Kw(Kw::Nothing) {
            return Ok(RetType::Nothing(self.bump().span));
        }
        if let Tok::Ident(_) = self.peek() {
            // `function add(...)` のように戻り値の型を書き忘れた。
            if self.peek_at(1) == &Tok::LParen {
                return Err(Diag::new(self.span(), "関数の戻り値の型がありません")
                    .hint("値を返さないなら `function nothing 名前(...)` と書きます"));
            }
        }
        Ok(RetType::Type(self.type_expr()?))
    }

    // ---------------- 型 ----------------

    fn type_expr(&mut self) -> PResult<TypeExpr> {
        let lo = self.span();
        let kind = match self.peek().clone() {
            Tok::Int(8) => {
                self.bump();
                TypeKind::Int(IntTy::U8)
            }
            Tok::Int(32) => {
                self.bump();
                TypeKind::Int(IntTy::I32)
            }
            Tok::Int(64) => {
                self.bump();
                TypeKind::Int(IntTy::I64)
            }
            Tok::Int(n) => {
                return Err(Diag::new(lo, format!("`{}` という型はありません", n))
                    .hint("整数の型は `8`、`32`、`64` です"))
            }
            Tok::Kw(Kw::Bool) => {
                self.bump();
                TypeKind::Bool
            }
            Tok::Kw(Kw::Decnum) => {
                self.bump();
                TypeKind::Decnum
            }
            Tok::Kw(Kw::String) => {
                self.bump();
                TypeKind::String
            }
            Tok::Kw(Kw::Address) => {
                self.bump();
                TypeKind::Address
            }
            Tok::Kw(k @ (Kw::Pointer | Kw::Array | Kw::Maybe)) => {
                self.bump();
                if self.peek() != &Tok::Colon {
                    return Err(Diag::new(
                        self.span(),
                        format!("`{}` の後に `:` と型が必要です", k.as_str()),
                    )
                    .hint(format!("例：{}:32", k.as_str())));
                }
                self.bump();
                let inner = Box::new(self.type_expr()?);
                match k {
                    Kw::Pointer => TypeKind::Pointer(inner),
                    Kw::Array => TypeKind::Array(inner),
                    _ => TypeKind::Maybe(inner),
                }
            }
            Tok::Kw(Kw::Function) => {
                self.bump();
                self.expect(Tok::Colon, "`function` の後に")?;
                let ret = self.ret_type()?;
                self.expect(Tok::LParen, "引数の型の始まりの")?;
                let mut params = Vec::new();
                if !self.eat(&Tok::RParen) {
                    loop {
                        params.push(self.type_expr()?);
                        if self.eat(&Tok::Comma) {
                            continue;
                        }
                        self.expect(Tok::RParen, "引数の型の終わりの")?;
                        break;
                    }
                }
                TypeKind::Function(Box::new(ret), params)
            }
            Tok::Ident(name) => {
                self.bump();
                // ジェネリクスの型引数（`List:32`）は、定義から数がわかってから読むので、今は付けない。
                TypeKind::Named(name, Vec::new())
            }
            _ => return Err(self.unexpected("型")),
        };
        Ok(TypeExpr {
            kind,
            span: lo.to(self.prev_span()),
        })
    }

    // ---------------- 文 ----------------

    fn block(&mut self) -> PResult<Block> {
        self.expect(Tok::LBrace, "ブロックの始まりの")?;
        let mut stmts = Vec::new();
        loop {
            match self.peek() {
                Tok::RBrace => break,
                Tok::Eof => {
                    return Err(Diag::new(self.span(), "`}` が足りません")
                        .hint("ブロックを `}` で閉じ忘れていませんか"))
                }
                _ => stmts.push(self.stmt()?),
            }
        }
        let close = self.bump().span;
        Ok(Block { stmts, close })
    }

    fn semi(&mut self) -> PResult<Span> {
        self.expect(Tok::Semi, "文の終わりの")
    }

    fn decl(&mut self) -> PResult<Decl> {
        let (name, name_span) = self.ident("変数の名前")?;
        if self.peek() != &Tok::Colon {
            return Err(
                Diag::new(self.span(), format!("変数 `{}` の型がありません", name))
                    .hint(format!("宣言は `{}:32 = 値;` のように `:` と型を書きます", name)),
            );
        }
        self.bump();
        let ty = self.type_expr()?;
        if self.peek() == &Tok::Semi {
            return Err(
                Diag::new(self.span(), format!("変数 `{}` に初期値がありません", name))
                    .hint(format!("`{}:... = 値;` のように初期値を書いてください", name)),
            );
        }
        self.expect(Tok::Assign, "初期値の")?;
        let init = self.expr()?;
        self.semi()?;
        Ok(Decl {
            name,
            name_span,
            ty,
            init,
        })
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        let lo = self.span();
        let kind = match self.peek() {
            Tok::Ident(_) if self.peek_at(1) == &Tok::Colon => StmtKind::Decl(self.decl()?),
            Tok::Kw(Kw::If) => return self.if_stmt(),
            Tok::Kw(Kw::While) => {
                self.bump();
                let cond = self.cond("while")?;
                let body = self.block()?;
                StmtKind::While { cond, body }
            }
            Tok::Kw(Kw::Break) => {
                self.bump();
                self.semi()?;
                StmtKind::Break
            }
            Tok::Kw(Kw::Continue) => {
                self.bump();
                self.semi()?;
                StmtKind::Continue
            }
            Tok::Kw(Kw::Return) => {
                self.bump();
                if self.eat(&Tok::Semi) {
                    StmtKind::Return(None)
                } else {
                    let e = self.expr()?;
                    self.semi()?;
                    StmtKind::Return(Some(e))
                }
            }
            Tok::Kw(Kw::Arena) => {
                self.bump();
                StmtKind::Arena(self.block()?)
            }
            Tok::Kw(Kw::Function) => {
                return Err(Diag::new(lo, "関数の中に関数は書けません")
                    .hint("関数はファイルのいちばん外側に書きます"))
            }
            _ => {
                let e = self.expr()?;
                if self.eat(&Tok::Assign) {
                    let value = self.expr()?;
                    self.semi()?;
                    StmtKind::Assign { target: e, value }
                } else {
                    self.semi()?;
                    StmtKind::Expr(e)
                }
            }
        };
        Ok(Stmt {
            kind,
            span: lo.to(self.prev_span()),
        })
    }

    /// `if` や `while` の `( 条件 )`。
    fn cond(&mut self, kw: &str) -> PResult<Expr> {
        if self.peek() != &Tok::LParen {
            return Err(
                Diag::new(self.span(), format!("`{}` の条件は `()` で囲みます", kw))
                    .hint(format!("例：{} (x > 3) {{ ... }}", kw)),
            );
        }
        self.bump();
        let e = self.expr()?;
        self.expect(Tok::RParen, "条件の終わりの")?;
        Ok(e)
    }

    fn if_stmt(&mut self) -> PResult<Stmt> {
        let lo = self.bump().span; // if
        let cond = self.cond("if")?;
        let then = self.block()?;
        let els = if self.eat(&Tok::Kw(Kw::Else)) {
            if self.peek() == &Tok::Kw(Kw::If) {
                Some(Else::If(Box::new(self.if_stmt()?)))
            } else {
                Some(Else::Block(self.block()?))
            }
        } else {
            None
        };
        Ok(Stmt {
            kind: StmtKind::If { cond, then, els },
            span: lo.to(self.prev_span()),
        })
    }

    // ---------------- 式 ----------------
    //
    // 優先順位（上ほど先に計算する）
    //   1  ()
    //   2  ^（右結合）
    //   3  not、符号の -
    //   4  * / %
    //   5  + -
    //   6  << >>
    //   7  < > <= >= == !=
    //   8  and nand
    //   9  xor xnor
    //  10  or nor

    pub fn expr(&mut self) -> PResult<Expr> {
        self.binary(10)
    }

    fn binop_at(&self, level: u8) -> Option<BinOp> {
        let op = match (level, self.peek()) {
            (10, Tok::Kw(Kw::Or)) => BinOp::Or,
            (10, Tok::Kw(Kw::Nor)) => BinOp::Nor,
            (9, Tok::Kw(Kw::Xor)) => BinOp::Xor,
            (9, Tok::Kw(Kw::Xnor)) => BinOp::Xnor,
            (8, Tok::Kw(Kw::And)) => BinOp::And,
            (8, Tok::Kw(Kw::Nand)) => BinOp::Nand,
            (7, Tok::Lt) => BinOp::Lt,
            (7, Tok::Gt) => BinOp::Gt,
            (7, Tok::Le) => BinOp::Le,
            (7, Tok::Ge) => BinOp::Ge,
            (7, Tok::EqEq) => BinOp::Eq,
            (7, Tok::Ne) => BinOp::Ne,
            (6, Tok::Shl) => BinOp::Shl,
            (6, Tok::Shr) => BinOp::Shr,
            (5, Tok::Plus) => BinOp::Add,
            (5, Tok::Minus) => BinOp::Sub,
            (4, Tok::Star) => BinOp::Mul,
            (4, Tok::Slash) => BinOp::Div,
            (4, Tok::Percent) => BinOp::Mod,
            _ => return None,
        };
        Some(op)
    }

    /// 順位 4〜10 の左結合の二項演算子。
    fn binary(&mut self, level: u8) -> PResult<Expr> {
        if level == 3 {
            return self.unary();
        }
        let mut lhs = self.binary(level - 1)?;
        while let Some(op) = self.binop_at(level) {
            self.bump();
            let rhs = self.binary(level - 1)?;
            let span = lhs.span.to(rhs.span);
            lhs = Expr {
                kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                span,
            };
        }
        Ok(lhs)
    }

    /// 順位 3：`not`、符号の `-`。
    fn unary(&mut self) -> PResult<Expr> {
        let lo = self.span();
        let op = match self.peek() {
            Tok::Kw(Kw::Not) => UnOp::Not,
            Tok::Minus => UnOp::Neg,
            _ => return self.power(),
        };
        self.bump();
        let e = self.unary()?;
        let span = lo.to(e.span);
        Ok(Expr {
            kind: ExprKind::Unary(op, Box::new(e)),
            span,
        })
    }

    /// 順位 2：`^`（右結合）。`-2^2` は `-(2^2)`、`2^-1` は `2^(-1)`。
    fn power(&mut self) -> PResult<Expr> {
        let base = self.postfix()?;
        if self.eat(&Tok::Caret) {
            let exp = self.unary()?;
            let span = base.span.to(exp.span);
            return Ok(Expr {
                kind: ExprKind::Binary(BinOp::Pow, Box::new(base), Box::new(exp)),
                span,
            });
        }
        Ok(base)
    }

    fn postfix(&mut self) -> PResult<Expr> {
        let mut e = self.primary()?;
        loop {
            match self.peek() {
                Tok::LParen => {
                    self.bump();
                    let args = self.args(Tok::RParen)?;
                    let span = e.span.to(self.prev_span());
                    e = Expr {
                        kind: ExprKind::Call(Box::new(e), args),
                        span,
                    };
                }
                Tok::LBracket => {
                    self.bump();
                    let idx = self.expr()?;
                    self.expect(Tok::RBracket, "添字の終わりの")?;
                    let span = e.span.to(self.prev_span());
                    e = Expr {
                        kind: ExprKind::Index(Box::new(e), Box::new(idx)),
                        span,
                    };
                }
                Tok::Dot => {
                    self.bump();
                    let (name, nspan) = self.ident("フィールドの名前")?;
                    let span = e.span.to(nspan);
                    e = Expr {
                        kind: ExprKind::Field(Box::new(e), name),
                        span,
                    };
                }
                _ => return Ok(e),
            }
        }
    }

    /// `,` で区切った式の並び。閉じ記号まで読む。
    fn args(&mut self, close: Tok) -> PResult<Vec<Expr>> {
        let mut args = Vec::new();
        if self.eat(&close) {
            return Ok(args);
        }
        loop {
            args.push(self.expr()?);
            if self.eat(&Tok::Comma) {
                continue;
            }
            let what = if close == Tok::RParen {
                "引数の終わりの"
            } else {
                "配列の終わりの"
            };
            self.expect(close, what)?;
            return Ok(args);
        }
    }

    fn primary(&mut self) -> PResult<Expr> {
        let lo = self.span();
        let kind = match self.peek().clone() {
            Tok::Int(n) => {
                self.bump();
                ExprKind::Int(n)
            }
            Tok::Float(f) => {
                self.bump();
                ExprKind::Float(f)
            }
            Tok::Str(s) => {
                self.bump();
                ExprKind::Str(s)
            }
            Tok::Char(c) => {
                self.bump();
                ExprKind::Char(c)
            }
            Tok::Kw(Kw::True) => {
                self.bump();
                ExprKind::Bool(true)
            }
            Tok::Kw(Kw::False) => {
                self.bump();
                ExprKind::Bool(false)
            }
            Tok::Kw(Kw::Null) => {
                self.bump();
                ExprKind::Null
            }
            Tok::Ident(name)
                if name == "size" && self.peek_at(1) == &Tok::LParen && self.peek_at(2) == &Tok::Colon =>
            {
                self.bump();
                self.bump();
                self.bump();
                let ty = self.type_expr()?;
                self.expect(Tok::RParen, "`size` の終わりの")?;
                ExprKind::SizeOf(ty)
            }
            Tok::Ident(name) => {
                self.bump();
                ExprKind::Var(name)
            }
            Tok::LParen => {
                self.bump();
                let e = self.expr()?;
                self.expect(Tok::RParen, "かっこを閉じる")?;
                return Ok(Expr {
                    kind: e.kind,
                    span: lo.to(self.prev_span()),
                });
            }
            Tok::LBracket => {
                self.bump();
                ExprKind::ArrayLit(self.args(Tok::RBracket)?)
            }
            Tok::Kw(Kw::To) => {
                self.bump();
                if self.peek() != &Tok::Colon {
                    return Err(Diag::new(self.span(), "`to` の後に `:` と型が必要です").hint("例：to:32(x)"));
                }
                self.bump();
                let ty = self.type_expr()?;
                self.expect(Tok::LParen, "変換する値の")?;
                let e = self.expr()?;
                self.expect(Tok::RParen, "変換の終わりの")?;
                ExprKind::Convert(ty, Box::new(e))
            }
            Tok::Kw(Kw::Array) if self.peek_at(1) == &Tok::LParen => {
                self.bump();
                self.bump();
                let n = self.expr()?;
                self.expect(Tok::RParen, "`array` の終わりの")?;
                ExprKind::ArrayNew(Box::new(n))
            }
            _ => return Err(self.unexpected("式")),
        };
        Ok(Expr {
            kind,
            span: lo.to(self.prev_span()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;

    fn parse_src(s: &str) -> Result<Program, Diag> {
        parse(lex(s).unwrap().tokens)
    }

    fn ret_expr(src: &str) -> String {
        let p = parse_src(&format!("function 32 main() {{ return {}; }}", src)).unwrap();
        let Item::Func(f) = &p.items[0] else { panic!() };
        let StmtKind::Return(Some(e)) = &f.body.stmts[0].kind else {
            panic!()
        };
        show(e)
    }

    fn show(e: &Expr) -> String {
        match &e.kind {
            ExprKind::Int(n) => n.to_string(),
            ExprKind::Var(v) => v.clone(),
            ExprKind::Unary(UnOp::Neg, x) => format!("(-{})", show(x)),
            ExprKind::Unary(UnOp::Not, x) => format!("(not {})", show(x)),
            ExprKind::Binary(op, a, b) => format!("({} {} {})", show(a), op.as_str(), show(b)),
            _ => "?".into(),
        }
    }

    #[test]
    fn precedence() {
        assert_eq!(ret_expr("1 + 2 * 3"), "(1 + (2 * 3))");
        assert_eq!(ret_expr("-2 ^ 2"), "(-(2 ^ 2))");
        assert_eq!(ret_expr("2 ^ 3 ^ 2"), "(2 ^ (3 ^ 2))");
        assert_eq!(ret_expr("2 ^ -1"), "(2 ^ (-1))");
        assert_eq!(ret_expr("1 - 2 - 3"), "((1 - 2) - 3)");
        assert_eq!(ret_expr("a or b and c"), "(a or (b and c))");
        assert_eq!(ret_expr("a xor b or c"), "((a xor b) or c)");
        assert_eq!(ret_expr("1 << 2 + 3"), "(1 << (2 + 3))");
        assert_eq!(ret_expr("not a < b"), "((not a) < b)");
    }

    #[test]
    fn newlines_free() {
        assert!(parse_src("function nothing main() { x :\n32\n= add(1,2)\n; }").is_ok());
    }

    #[test]
    fn errors() {
        let e = parse_src("function 32 main() { return 1\n}").unwrap_err();
        assert!(e.msg.contains("`;`"));
        assert!(parse_src("function nothing main() { if x > 3 { } }")
            .unwrap_err()
            .msg
            .contains("()"));
        assert!(parse_src("function nothing main() { x:32; }")
            .unwrap_err()
            .msg
            .contains("初期値"));
        assert!(parse_src("function main() { }")
            .unwrap_err()
            .msg
            .contains("戻り値の型"));
        assert!(parse_src("function :32 main() { }").is_err());
    }

    #[test]
    fn types() {
        let p = parse_src(
            "function pointer:pointer:32 f(a:array:maybe:string, g:function:32(32, 8)) { return a; }",
        )
        .unwrap();
        let Item::Func(f) = &p.items[0] else { panic!() };
        assert_eq!(f.params.len(), 2);
    }

    #[test]
    fn else_if() {
        assert!(parse_src("function nothing main() { if (a) { } else if (b) { } else { } }").is_ok());
    }
}
