//! 型の確認と、中間表現への変換。
//!
//! 今できるのは手順 4 まで：`32` と `bool`、変数、`if`、`while`、関数呼び出し、グローバル変数。
//! それ以外の書き方は、構文としては読めても「まだ実装していません」のエラーにする。

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::fold;
use crate::ir::{self, CmpOp, Inst, IrBinOp, Operand, Place};
use crate::source::{Diag, Span};

/// 値の型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    Int(IntTy),
    Bool,
}

impl Ty {
    fn name(self) -> &'static str {
        match self {
            Ty::Int(t) => t.name(),
            Ty::Bool => "bool",
        }
    }
}

const I32: Ty = Ty::Int(IntTy::I32);

fn not_yet(span: Span, what: &str, step: u32) -> Diag {
    Diag::new(
        span,
        format!("{}はまだ実装していません（実装の手順 {} で対応）", what, step),
    )
}

/// ソースに書かれた型を確かめる。今使える型は `32` と `bool`。
fn resolve_type(t: &TypeExpr) -> Result<Ty, Diag> {
    match &t.kind {
        TypeKind::Int(IntTy::I32) => Ok(I32),
        TypeKind::Bool => Ok(Ty::Bool),
        TypeKind::Int(IntTy::U8) => Err(not_yet(t.span, "`8` 型", 6)),
        TypeKind::Int(IntTy::I64) => Err(not_yet(t.span, "`64` 型", 6)),
        TypeKind::Decnum => Err(not_yet(t.span, "`decnum` 型", 6)),
        TypeKind::String | TypeKind::Pointer(_) | TypeKind::Array(_) | TypeKind::Address => {
            Err(not_yet(t.span, "この型", 5))
        }
        TypeKind::Maybe(_) | TypeKind::Named(..) => Err(not_yet(t.span, "この型", 6)),
        TypeKind::Function(..) => Err(not_yet(t.span, "関数ポインタ", 7)),
    }
}

// ---------------- プログラム全体 ----------------

#[derive(Clone)]
struct FuncSig {
    name: String,
    params: Vec<Ty>,
    /// `None` は `nothing`。
    ret: Option<Ty>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GlobalState {
    Todo,
    /// 初期値を計算している途中（自分自身を使っていたら見つけるため）。
    Busy,
    Done(i64),
}

struct GlobalInfo<'p> {
    decl: &'p Decl,
    ty: Ty,
    state: GlobalState,
}

#[derive(Clone, Copy)]
enum TopName {
    Func(usize),
    Global(usize),
}

struct Ctx<'p> {
    funcs: Vec<FuncSig>,
    globals: Vec<GlobalInfo<'p>>,
    names: HashMap<&'p str, TopName>,
}

pub fn lower(prog: &Program) -> Result<ir::Program, Diag> {
    let mut ctx = Ctx {
        funcs: Vec::new(),
        globals: Vec::new(),
        names: HashMap::new(),
    };
    let mut func_asts: Vec<&Func> = Vec::new();

    // 1. 名前を集める。関数は定義より前の行からでも呼べる。
    for item in &prog.items {
        let (name, span, top) = match item {
            Item::Func(f) => (&f.name, f.name_span, TopName::Func(func_asts.len())),
            Item::Global(d) => (&d.name, d.name_span, TopName::Global(ctx.globals.len())),
        };
        if ctx.names.insert(name, top).is_some() {
            return Err(
                Diag::new(span, format!("`{}` という名前が 2 回定義されています", name))
                    .hint("関数とグローバル変数は、すべて違う名前にします"),
            );
        }
        match item {
            Item::Func(f) => func_asts.push(f),
            Item::Global(d) => {
                let ty = resolve_type(&d.ty)?;
                ctx.globals.push(GlobalInfo {
                    decl: d,
                    ty,
                    state: GlobalState::Todo,
                });
            }
        }
    }

    // 2. 関数の引数と戻り値の型。
    for f in &func_asts {
        let mut seen = HashSet::new();
        let mut params = Vec::new();
        for p in &f.params {
            if !seen.insert(p.name.as_str()) {
                return Err(Diag::new(p.span, format!("引数 `{}` が 2 つあります", p.name)));
            }
            if let Some(top) = ctx.names.get(p.name.as_str()) {
                return Err(top_name_conflict(p.span, &p.name, *top));
            }
            params.push(resolve_type(&p.ty)?);
        }
        let ret = match &f.ret {
            RetType::Nothing(_) => None,
            RetType::Type(t) => Some(resolve_type(t)?),
        };
        ctx.funcs.push(FuncSig {
            name: f.name.clone(),
            params,
            ret,
        });
    }

    // 3. グローバル変数の初期値（コンパイル時に計算する）。
    for i in 0..ctx.globals.len() {
        ctx.eval_global(i)?;
    }

    // 4. main
    let main = match ctx.names.get("main") {
        Some(TopName::Func(i)) => *i,
        Some(TopName::Global(i)) => {
            return Err(Diag::new(
                ctx.globals[*i].decl.name_span,
                "`main` は関数の名前に使います",
            ))
        }
        None => {
            return Err(Diag::new(Span::new(0, 0), "`main` 関数がありません")
                .hint("プログラムは `function nothing main() { ... }` から始まります"))
        }
    };
    let main_returns = check_main(func_asts[main], &ctx.funcs[main])?;

    // 5. 関数の中身。
    let mut funcs = Vec::new();
    for (i, f) in func_asts.iter().enumerate() {
        funcs.push(lower_func(&mut ctx, i, f)?);
    }
    let globals = ctx
        .globals
        .iter()
        .map(|g| ir::Global {
            name: g.decl.name.clone(),
            init: match g.state {
                GlobalState::Done(v) => v,
                _ => unreachable!("グローバル変数はすべて計算済み"),
            },
        })
        .collect();
    Ok(ir::Program {
        funcs,
        globals,
        main: main as u32,
        main_returns,
    })
}

fn top_name_conflict(span: Span, name: &str, top: TopName) -> Diag {
    let what = match top {
        TopName::Func(_) => "関数",
        TopName::Global(_) => "グローバル変数",
    };
    Diag::new(
        span,
        format!("`{}` は{}の名前なので、変数の名前には使えません", name, what),
    )
    .hint("外側にある名前と同じ名前は付けられません。別の名前にしてください")
}

/// `main` の形を確かめる。値を返すなら true。
fn check_main(f: &Func, sig: &FuncSig) -> Result<bool, Diag> {
    if let Some(p) = f.params.first() {
        return Err(not_yet(p.span, "`main` の引数", 5));
    }
    match sig.ret {
        None => Ok(false),
        Some(I32) => Ok(true),
        Some(_) => Err(Diag::new(
            f.ret.span(),
            "`main` の戻り値の型は `32` か `nothing` です",
        )),
    }
}

impl<'p> Ctx<'p> {
    /// グローバル変数の初期値を計算する。`immutable` の変数を使っていれば、先にそれを計算する。
    fn eval_global(&mut self, i: usize) -> Result<i64, Diag> {
        let decl = self.globals[i].decl;
        match self.globals[i].state {
            GlobalState::Done(v) => return Ok(v),
            GlobalState::Busy => {
                return Err(Diag::new(
                    decl.name_span,
                    format!(
                        "グローバル変数 `{}` の初期値が、自分自身を使っています",
                        decl.name
                    ),
                )
                .hint("immutable どうしで、お互いの値を使い合っていませんか"))
            }
            GlobalState::Todo => {}
        }
        self.globals[i].state = GlobalState::Busy;
        let ty = self.globals[i].ty;
        let mut l = FnLower::new(self, None, true);
        let op = l.expr_as(&decl.init, ty)?;
        let mut insts = std::mem::take(&mut l.insts);
        let v = fold::fold_const(&mut insts, op)?.ok_or_else(|| {
            Diag::new(
                decl.init.span,
                "グローバル変数の初期値には、値がコンパイル時に決まる式だけが書けます",
            )
        })?;
        self.globals[i].state = GlobalState::Done(v);
        Ok(v)
    }
}

// ---------------- 関数の中 ----------------

struct Local {
    name: String,
    var: u32,
    ty: Ty,
}

struct FnLower<'c, 'p> {
    ctx: &'c mut Ctx<'p>,
    /// 関数の戻り値の型。`None` は `nothing`。
    ret: Option<Ty>,
    /// グローバル変数の初期値を計算しているところか。
    const_ctx: bool,
    insts: Vec<Inst>,
    ntemps: u32,
    nvars: u32,
    nlabels: u32,
    scopes: Vec<Vec<Local>>,
    /// `while` の（先頭、出口）のラベル。
    loops: Vec<(u32, u32)>,
    /// 関数の中で宣言される名前すべて（エラーメッセージ用）。
    declared_somewhere: HashSet<String>,
}

fn lower_func(ctx: &mut Ctx, index: usize, f: &Func) -> Result<ir::Function, Diag> {
    let sig = ctx.funcs[index].clone();
    let mut l = FnLower::new(ctx, sig.ret, false);
    collect_decls(&f.body, &mut l.declared_somewhere);
    l.scopes.push(Vec::new());
    for (p, ty) in f.params.iter().zip(&sig.params) {
        let var = l.new_var();
        l.scopes[0].push(Local {
            name: p.name.clone(),
            var,
            ty: *ty,
        });
    }
    l.block(&f.body)?;
    if sig.ret.is_some() {
        if !block_returns(&f.body) {
            return Err(Diag::new(
                f.body.close,
                format!("関数 `{}` が値を返さずに終わることがあります", f.name),
            )
            .hint("どの道を通っても `return 値;` に着くようにしてください"));
        }
    } else {
        l.insts.push(Inst::Ret(None));
    }
    Ok(ir::Function {
        name: f.name.clone(),
        returns_value: sig.ret.is_some(),
        nparams: sig.params.len() as u32,
        insts: l.insts,
        ntemps: l.ntemps,
        nvars: l.nvars,
    })
}

fn collect_decls(b: &Block, out: &mut HashSet<String>) {
    for s in &b.stmts {
        collect_decls_stmt(s, out);
    }
}

fn collect_decls_stmt(s: &Stmt, out: &mut HashSet<String>) {
    match &s.kind {
        StmtKind::Decl(d) => {
            out.insert(d.name.clone());
        }
        StmtKind::If { then, els, .. } => {
            collect_decls(then, out);
            match els {
                Some(Else::Block(b)) => collect_decls(b, out),
                Some(Else::If(s)) => collect_decls_stmt(s, out),
                None => {}
            }
        }
        StmtKind::While { body, .. } | StmtKind::Arena(body) => collect_decls(body, out),
        _ => {}
    }
}

/// ブロックの終わりまで来ずに、必ず `return` するか（または抜け出せない）。
fn block_returns(b: &Block) -> bool {
    b.stmts.iter().any(stmt_returns)
}

fn stmt_returns(s: &Stmt) -> bool {
    match &s.kind {
        StmtKind::Return(_) => true,
        StmtKind::If {
            then, els: Some(els), ..
        } => {
            block_returns(then)
                && match els {
                    Else::Block(b) => block_returns(b),
                    Else::If(s) => stmt_returns(s),
                }
        }
        StmtKind::Arena(b) => block_returns(b),
        // `while (true)` は、中に（この while を抜ける）`break` がなければ終わらない。
        StmtKind::While { cond, body } => matches!(cond.kind, ExprKind::Bool(true)) && !has_break(body),
        _ => false,
    }
}

/// このループを抜ける `break` があるか（内側の while の中の break は数えない）。
fn has_break(b: &Block) -> bool {
    b.stmts.iter().any(stmt_has_break)
}

fn stmt_has_break(s: &Stmt) -> bool {
    match &s.kind {
        StmtKind::Break => true,
        StmtKind::If { then, els, .. } => {
            has_break(then)
                || match els {
                    Some(Else::Block(b)) => has_break(b),
                    Some(Else::If(s)) => stmt_has_break(s),
                    None => false,
                }
        }
        StmtKind::Arena(b) => has_break(b),
        _ => false,
    }
}

fn is_comparison(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge | BinOp::Eq | BinOp::Ne
    )
}

fn is_logic(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::And | BinOp::Nand | BinOp::Or | BinOp::Nor | BinOp::Xor | BinOp::Xnor
    )
}

fn mismatch(span: Span, want: Ty, got: Ty) -> Diag {
    let d = Diag::new(
        span,
        format!(
            "`{}` 型の値が必要ですが、この式は `{}` 型です",
            want.name(),
            got.name()
        ),
    );
    match (want, got) {
        (Ty::Bool, Ty::Int(_)) => d.hint("整数を条件に使うなら、`x != 0` のように比べてください"),
        _ => d,
    }
}

impl<'c, 'p> FnLower<'c, 'p> {
    fn new(ctx: &'c mut Ctx<'p>, ret: Option<Ty>, const_ctx: bool) -> Self {
        FnLower {
            ctx,
            ret,
            const_ctx,
            insts: Vec::new(),
            ntemps: 0,
            nvars: 0,
            nlabels: 0,
            scopes: Vec::new(),
            loops: Vec::new(),
            declared_somewhere: HashSet::new(),
        }
    }

    fn temp(&mut self) -> u32 {
        self.ntemps += 1;
        self.ntemps - 1
    }

    fn new_var(&mut self) -> u32 {
        self.nvars += 1;
        self.nvars - 1
    }

    fn label(&mut self) -> u32 {
        self.nlabels += 1;
        self.nlabels - 1
    }

    fn emit(&mut self, i: Inst) {
        self.insts.push(i);
    }

    fn lookup(&self, name: &str) -> Option<&Local> {
        self.scopes
            .iter()
            .rev()
            .flat_map(|s| s.iter())
            .find(|l| l.name == name)
    }

    fn declare(&mut self, name: &str, span: Span, ty: Ty) -> Result<u32, Diag> {
        if self.scopes.last().unwrap().iter().any(|l| l.name == name) {
            return Err(Diag::new(
                span,
                format!("変数 `{}` はこのブロックでもう宣言されています", name),
            )
            .hint("値を変えるなら、`:` と型を付けずに `名前 = 値;` と書きます"));
        }
        if self.lookup(name).is_some() {
            return Err(
                Diag::new(span, format!("外側に同じ名前の変数 `{}` があります", name))
                    .hint("外側の変数と同じ名前は付けられません。別の名前にしてください"),
            );
        }
        if let Some(top) = self.ctx.names.get(name) {
            return Err(top_name_conflict(span, name, *top));
        }
        let var = self.new_var();
        self.scopes.last_mut().unwrap().push(Local {
            name: name.to_string(),
            var,
            ty,
        });
        Ok(var)
    }

    // ---------------- 文 ----------------

    fn block(&mut self, b: &Block) -> Result<(), Diag> {
        self.scopes.push(Vec::new());
        for s in &b.stmts {
            self.stmt(s)?;
        }
        self.scopes.pop();
        Ok(())
    }

    fn stmt(&mut self, s: &Stmt) -> Result<(), Diag> {
        match &s.kind {
            StmtKind::Decl(d) => {
                let ty = resolve_type(&d.ty)?;
                // 初期値の中では、まだ自分自身は使えない。
                let v = self.expr_as(&d.init, ty)?;
                let var = self.declare(&d.name, d.name_span, ty)?;
                self.emit(Inst::Store {
                    place: Place::Var(var),
                    src: v,
                });
            }
            StmtKind::Assign { target, value } => self.assign(target, value)?,
            StmtKind::If { cond, then, els } => self.if_stmt(cond, then, els.as_ref())?,
            StmtKind::While { cond, body } => {
                let head = self.label();
                let body_l = self.label();
                let end = self.label();
                self.emit(Inst::Label(head));
                let c = self.cond(cond)?;
                self.emit(Inst::Branch {
                    cond: c,
                    then: body_l,
                    els: end,
                });
                self.emit(Inst::Label(body_l));
                self.loops.push((head, end));
                self.block(body)?;
                self.loops.pop();
                self.emit(Inst::Jump(head));
                self.emit(Inst::Label(end));
            }
            StmtKind::Break | StmtKind::Continue => {
                let is_break = matches!(s.kind, StmtKind::Break);
                let Some(&(head, end)) = self.loops.last() else {
                    let kw = if is_break { "break" } else { "continue" };
                    return Err(Diag::new(
                        s.span,
                        format!("`{}` は `while` の中でだけ使えます", kw),
                    ));
                };
                self.emit(Inst::Jump(if is_break { end } else { head }));
            }
            StmtKind::Return(None) => {
                if self.ret.is_some() {
                    return Err(Diag::new(s.span, "値を返す関数なので、`return` に値が必要です"));
                }
                self.emit(Inst::Ret(None));
            }
            StmtKind::Return(Some(e)) => {
                let Some(ty) = self.ret else {
                    return Err(Diag::new(e.span, "`nothing` の関数では値を `return` できません")
                        .hint("値を返すなら、`function 32 名前(...)` のように戻り値の型を書きます"));
                };
                let v = self.expr_as(e, ty)?;
                self.emit(Inst::Ret(Some(v)));
            }
            StmtKind::Arena(_) => return Err(not_yet(s.span, "`arena`", 7)),
            StmtKind::Expr(e) => match &e.kind {
                ExprKind::Call(callee, args) => {
                    self.call(e, callee, args)?;
                }
                ExprKind::Binary(BinOp::Eq, ..) => {
                    return Err(
                        Diag::new(e.span, "この式は文として使えません（比べた結果が使われません）")
                            .hint("代入するなら `==` ではなく `=` と書きます"),
                    )
                }
                _ => {
                    return Err(
                        Diag::new(e.span, "この式は文として使えません（計算した値が使われません）")
                            .hint("文として書けるのは、宣言、代入、関数の呼び出しなどです"),
                    )
                }
            },
        }
        Ok(())
    }

    fn assign(&mut self, target: &Expr, value: &Expr) -> Result<(), Diag> {
        let ExprKind::Var(name) = &target.kind else {
            return match &target.kind {
                ExprKind::Index(..) => Err(not_yet(target.span, "添字への代入", 5)),
                ExprKind::Field(..) => Err(not_yet(target.span, "フィールドへの代入", 6)),
                _ => Err(Diag::new(target.span, "ここには代入できません")
                    .hint("`=` の左には変数を書きます。比べるなら `==` です")),
            };
        };
        let (place, ty) = if let Some(l) = self.lookup(name) {
            (Place::Var(l.var), l.ty)
        } else {
            match self.ctx.names.get(name.as_str()) {
                Some(TopName::Global(i)) => {
                    let g = &self.ctx.globals[*i];
                    if g.decl.immutable {
                        return Err(Diag::new(
                            target.span,
                            format!("`{}` は immutable なので、値を変えられません", name),
                        ));
                    }
                    (Place::Global(*i as u32), g.ty)
                }
                Some(TopName::Func(_)) => {
                    return Err(Diag::new(
                        target.span,
                        format!("`{}` は関数なので、代入できません", name),
                    ))
                }
                None => return Err(self.unknown_var(target.span, name)),
            }
        };
        let v = self.expr_as(value, ty)?;
        self.emit(Inst::Store { place, src: v });
        Ok(())
    }

    fn if_stmt(&mut self, cond: &Expr, then: &Block, els: Option<&Else>) -> Result<(), Diag> {
        let c = self.cond(cond)?;
        let then_l = self.label();
        let else_l = self.label();
        let end = self.label();
        self.emit(Inst::Branch {
            cond: c,
            then: then_l,
            els: else_l,
        });
        self.emit(Inst::Label(then_l));
        self.block(then)?;
        self.emit(Inst::Jump(end));
        self.emit(Inst::Label(else_l));
        match els {
            Some(Else::Block(b)) => self.block(b)?,
            Some(Else::If(s)) => self.stmt(s)?,
            None => {}
        }
        self.emit(Inst::Label(end));
        Ok(())
    }

    /// `if` や `while` の条件。`bool` だけ。
    fn cond(&mut self, e: &Expr) -> Result<Operand, Diag> {
        let (v, t) = self.expr(e, Some(Ty::Bool))?;
        if t != Ty::Bool {
            return Err(Diag::new(
                e.span,
                format!(
                    "条件に書けるのは `bool` の値だけですが、この式は `{}` 型です",
                    t.name()
                ),
            )
            .hint("整数なら `x != 0` のように比べてください"));
        }
        Ok(v)
    }

    // ---------------- 式 ----------------

    /// 式を `ty` 型の値として計算する。型が違えばエラー。
    fn expr_as(&mut self, e: &Expr, ty: Ty) -> Result<Operand, Diag> {
        let (v, t) = self.expr(e, Some(ty))?;
        if t != ty {
            return Err(mismatch(e.span, ty, t));
        }
        Ok(v)
    }

    /// 式の型を、計算せずに調べる。整数リテラルだけの式など、文脈で決まるものは `None`。
    fn peek(&self, e: &Expr) -> Option<Ty> {
        match &e.kind {
            ExprKind::Bool(_) => Some(Ty::Bool),
            ExprKind::Var(name) => {
                if let Some(l) = self.lookup(name) {
                    return Some(l.ty);
                }
                match self.ctx.names.get(name.as_str()) {
                    Some(TopName::Global(i)) => Some(self.ctx.globals[*i].ty),
                    _ => None,
                }
            }
            ExprKind::Unary(_, x) => self.peek(x),
            ExprKind::Binary(op, a, b) => {
                if is_comparison(*op) {
                    Some(Ty::Bool)
                } else {
                    self.peek(a).or_else(|| self.peek(b))
                }
            }
            ExprKind::Call(callee, _) => match &callee.kind {
                ExprKind::Var(name) if self.lookup(name).is_none() => match self.ctx.names.get(name.as_str())
                {
                    Some(TopName::Func(i)) => self.ctx.funcs[*i].ret,
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        }
    }

    /// 式を計算する。`want` は文脈から期待される型（整数リテラルの型を決めるのに使う）。
    fn expr(&mut self, e: &Expr, want: Option<Ty>) -> Result<(Operand, Ty), Diag> {
        match &e.kind {
            ExprKind::Int(n) => {
                let t = match want {
                    Some(Ty::Int(t)) => t,
                    _ => IntTy::I32,
                };
                Ok((int_literal(*n, false, t, e.span)?, Ty::Int(t)))
            }
            ExprKind::Bool(b) => Ok((Operand::Imm(*b as i64), Ty::Bool)),
            ExprKind::Var(name) => self.var(e.span, name),
            ExprKind::Unary(UnOp::Neg, inner) => {
                // `-2147483648` は、2147483648 が 32 に入らなくても書けるようにする。
                if let ExprKind::Int(n) = inner.kind {
                    let t = match want {
                        Some(Ty::Int(t)) => t,
                        _ => IntTy::I32,
                    };
                    return Ok((int_literal(n, true, t, e.span)?, Ty::Int(t)));
                }
                let (a, t) = self.expr(inner, want)?;
                let Ty::Int(it) = t else {
                    return Err(Diag::new(e.span, "符号の `-` は `bool` には使えません")
                        .hint("真と偽を入れ替えるなら `not` を使います"));
                };
                let dst = self.temp();
                self.emit(Inst::Neg {
                    dst,
                    ty: it,
                    a,
                    span: e.span,
                });
                Ok((Operand::Temp(dst), t))
            }
            ExprKind::Unary(UnOp::Not, inner) => {
                let (a, t) = self.expr(inner, want)?;
                let dst = self.temp();
                match t {
                    Ty::Bool => self.emit(Inst::BoolNot { dst, a }),
                    Ty::Int(it) => self.emit(Inst::Not { dst, ty: it, a }),
                }
                Ok((Operand::Temp(dst), t))
            }
            ExprKind::Binary(op, a, b) => self.binary(e, *op, a, b, want),
            ExprKind::Call(callee, args) => match self.call(e, callee, args)? {
                Some(v) => Ok(v),
                None => {
                    let name = match &callee.kind {
                        ExprKind::Var(n) => n.as_str(),
                        _ => "?",
                    };
                    Err(Diag::new(
                        e.span,
                        format!(
                            "関数 `{}` は値を返さない（`nothing`）ので、値としては使えません",
                            name
                        ),
                    ))
                }
            },
            ExprKind::Char(_) => Err(not_yet(e.span, "文字コード（`8` 型）", 6)),
            ExprKind::Float(_) => Err(not_yet(e.span, "小数（`decnum` 型）", 6)),
            ExprKind::Str(_) => Err(not_yet(e.span, "文字列", 5)),
            ExprKind::Null => Err(not_yet(e.span, "`null`", 6)),
            ExprKind::Index(..) | ExprKind::ArrayLit(_) | ExprKind::ArrayNew(_) => {
                Err(not_yet(e.span, "配列", 5))
            }
            ExprKind::Field(..) => Err(not_yet(e.span, "`.` を使う書き方（フィールドや名前空間）", 5)),
            ExprKind::Convert(..) => Err(not_yet(e.span, "`to:型(…)`", 6)),
            ExprKind::SizeOf(..) => Err(not_yet(e.span, "`size(…)`", 5)),
        }
    }

    fn var(&mut self, span: Span, name: &str) -> Result<(Operand, Ty), Diag> {
        if let Some(l) = self.lookup(name) {
            let (var, ty) = (l.var, l.ty);
            let dst = self.temp();
            self.emit(Inst::Copy {
                dst,
                src: Operand::Var(var),
            });
            return Ok((Operand::Temp(dst), ty));
        }
        match self.ctx.names.get(name) {
            Some(TopName::Global(i)) => {
                let i = *i;
                let (ty, immutable) = (self.ctx.globals[i].ty, self.ctx.globals[i].decl.immutable);
                if immutable {
                    // immutable の値はコンパイル時に決まっているので、そのまま埋め込む。
                    let v = self.ctx.eval_global(i)?;
                    return Ok((Operand::Imm(v), ty));
                }
                if self.const_ctx {
                    return Err(Diag::new(
                        span,
                        format!(
                            "値の変わるグローバル変数 `{}` は、グローバル変数の初期値には使えません",
                            name
                        ),
                    )
                    .hint(format!("`{}` に `immutable` を付けると使えます", name)));
                }
                // 読んだ時点の値を一時変数に写す（後の関数呼び出しで書き換わっても影響しないように）。
                let dst = self.temp();
                self.emit(Inst::Copy {
                    dst,
                    src: Operand::Global(i as u32),
                });
                Ok((Operand::Temp(dst), ty))
            }
            Some(TopName::Func(_)) => Err(not_yet(span, "関数を値として使うこと（関数ポインタ）", 7)),
            None => Err(self.unknown_var(span, name)),
        }
    }

    fn unknown_var(&self, span: Span, name: &str) -> Diag {
        if self.declared_somewhere.contains(name) {
            Diag::new(span, format!("変数 `{}` はここでは使えません", name))
                .hint("宣言より前か、宣言したブロック `{ }` の外で使っています")
        } else {
            Diag::new(span, format!("`{}` という変数はありません", name))
        }
    }

    fn binary(
        &mut self,
        e: &Expr,
        op: BinOp,
        a: &Expr,
        b: &Expr,
        want: Option<Ty>,
    ) -> Result<(Operand, Ty), Diag> {
        if is_comparison(op) {
            let t = self.peek(a).or_else(|| self.peek(b)).unwrap_or(I32);
            let av = self.expr_as(a, t)?;
            let bv = self.expr_as(b, t)?;
            if t == Ty::Bool && !matches!(op, BinOp::Eq | BinOp::Ne) {
                return Err(Diag::new(
                    e.span,
                    format!("`bool` は `{}` で大小を比べられません", op.as_str()),
                ));
            }
            let cop = match op {
                BinOp::Lt => CmpOp::Lt,
                BinOp::Gt => CmpOp::Gt,
                BinOp::Le => CmpOp::Le,
                BinOp::Ge => CmpOp::Ge,
                BinOp::Eq => CmpOp::Eq,
                _ => CmpOp::Ne,
            };
            let dst = self.temp();
            self.emit(Inst::Cmp {
                dst,
                op: cop,
                a: av,
                b: bv,
            });
            return Ok((Operand::Temp(dst), Ty::Bool));
        }

        let want_int = want.filter(|w| matches!(w, Ty::Int(_)));
        let t = if is_logic(op) {
            self.peek(a).or_else(|| self.peek(b)).or(want).unwrap_or(I32)
        } else {
            self.peek(a).or_else(|| self.peek(b)).or(want_int).unwrap_or(I32)
        };
        let it = match t {
            Ty::Int(it) => it,
            Ty::Bool if is_logic(op) => return self.logic_bool(op, a, b),
            Ty::Bool => {
                return Err(
                    Diag::new(e.span, format!("`{}` は `bool` には使えません", op.as_str()))
                        .hint("真偽の演算には `and`・`or`・`not` などを使います"),
                )
            }
        };
        let av = self.expr_as(a, t)?;
        let bv = self.expr_as(b, t)?;
        let irop = match op {
            BinOp::Add => IrBinOp::Add,
            BinOp::Sub => IrBinOp::Sub,
            BinOp::Mul => IrBinOp::Mul,
            BinOp::Div => IrBinOp::Div,
            BinOp::Mod => IrBinOp::Mod,
            BinOp::Pow => IrBinOp::Pow,
            BinOp::Shl => IrBinOp::Shl,
            BinOp::Shr => IrBinOp::Shr,
            BinOp::And => IrBinOp::And,
            BinOp::Nand => IrBinOp::Nand,
            BinOp::Or => IrBinOp::Or,
            BinOp::Nor => IrBinOp::Nor,
            BinOp::Xor => IrBinOp::Xor,
            BinOp::Xnor => IrBinOp::Xnor,
            _ => unreachable!("比較は上で処理済み"),
        };
        let dst = self.temp();
        self.emit(Inst::Bin {
            dst,
            op: irop,
            ty: it,
            a: av,
            b: bv,
            span: e.span,
        });
        Ok((Operand::Temp(dst), t))
    }

    /// `bool` の `and`・`or` など。`and`・`nand`・`or`・`nor` は、左辺で結果が決まれば右辺を計算しない。
    fn logic_bool(&mut self, op: BinOp, a: &Expr, b: &Expr) -> Result<(Operand, Ty), Diag> {
        let av = self.expr_as(a, Ty::Bool)?;
        if matches!(op, BinOp::Xor | BinOp::Xnor) {
            let bv = self.expr_as(b, Ty::Bool)?;
            let dst = self.temp();
            let cop = if op == BinOp::Xor { CmpOp::Ne } else { CmpOp::Eq };
            self.emit(Inst::Cmp {
                dst,
                op: cop,
                a: av,
                b: bv,
            });
            return Ok((Operand::Temp(dst), Ty::Bool));
        }
        let negate = matches!(op, BinOp::Nand | BinOp::Nor);
        if self.const_ctx {
            // 初期値の計算では副作用がないので、両辺を計算してよい。
            let bv = self.expr_as(b, Ty::Bool)?;
            let dst = self.temp();
            let irop = if matches!(op, BinOp::And | BinOp::Nand) {
                IrBinOp::And
            } else {
                IrBinOp::Or
            };
            self.emit(Inst::Bin {
                dst,
                op: irop,
                ty: IntTy::I32,
                a: av,
                b: bv,
                span: a.span.to(b.span),
            });
            if negate {
                let n = self.temp();
                self.emit(Inst::BoolNot {
                    dst: n,
                    a: Operand::Temp(dst),
                });
                return Ok((Operand::Temp(n), Ty::Bool));
            }
            return Ok((Operand::Temp(dst), Ty::Bool));
        }
        let result = self.new_var();
        let rhs = self.label();
        let short = self.label();
        let end = self.label();
        let is_and = matches!(op, BinOp::And | BinOp::Nand);
        // and：左が偽なら結果は決まる。or：左が真なら結果は決まる。
        if is_and {
            self.emit(Inst::Branch {
                cond: av,
                then: rhs,
                els: short,
            });
        } else {
            self.emit(Inst::Branch {
                cond: av,
                then: short,
                els: rhs,
            });
        }
        self.emit(Inst::Label(short));
        let short_value = (!is_and) ^ negate;
        self.emit(Inst::Store {
            place: Place::Var(result),
            src: Operand::Imm(short_value as i64),
        });
        self.emit(Inst::Jump(end));
        self.emit(Inst::Label(rhs));
        let mut bv = self.expr_as(b, Ty::Bool)?;
        if negate {
            let n = self.temp();
            self.emit(Inst::BoolNot { dst: n, a: bv });
            bv = Operand::Temp(n);
        }
        self.emit(Inst::Store {
            place: Place::Var(result),
            src: bv,
        });
        self.emit(Inst::Label(end));
        let dst = self.temp();
        self.emit(Inst::Copy {
            dst,
            src: Operand::Var(result),
        });
        Ok((Operand::Temp(dst), Ty::Bool))
    }

    /// 関数を呼ぶ。`nothing` の関数なら `None`。
    fn call(&mut self, e: &Expr, callee: &Expr, args: &[Expr]) -> Result<Option<(Operand, Ty)>, Diag> {
        let name = match &callee.kind {
            ExprKind::Var(n) => n,
            ExprKind::Field(..) => return Err(not_yet(callee.span, "`import` したライブラリの関数", 5)),
            _ => return Err(not_yet(callee.span, "この形の関数呼び出し", 7)),
        };
        if self.const_ctx {
            return Err(Diag::new(e.span, "グローバル変数の初期値では、関数を呼べません")
                .hint("初期値には、値がコンパイル時に決まる式だけが書けます"));
        }
        if self.lookup(name).is_some() {
            return Err(not_yet(
                callee.span,
                "変数に入った関数の呼び出し（関数ポインタ）",
                7,
            ));
        }
        let index = match self.ctx.names.get(name.as_str()) {
            Some(TopName::Func(i)) => *i,
            Some(TopName::Global(_)) => {
                return Err(Diag::new(
                    callee.span,
                    format!("`{}` はグローバル変数なので、呼べません", name),
                ))
            }
            None => {
                let d = Diag::new(callee.span, format!("`{}` という関数はありません", name));
                return Err(if name == "print" || name == "put" {
                    d.hint("表示は `import stdio;` と `stdio.print(…)` で行います（実装の手順 5 で対応）")
                } else {
                    d
                });
            }
        };
        let sig = self.ctx.funcs[index].clone();
        if args.len() != sig.params.len() {
            return Err(Diag::new(
                e.span,
                format!(
                    "関数 `{}` の引数は {} 個ですが、{} 個書かれています",
                    sig.name,
                    sig.params.len(),
                    args.len()
                ),
            ));
        }
        let mut ops = Vec::new();
        for (arg, ty) in args.iter().zip(&sig.params) {
            ops.push(self.expr_as(arg, *ty)?);
        }
        let dst = sig.ret.map(|_| self.temp());
        self.emit(Inst::Call {
            dst,
            func: index as u32,
            args: ops,
        });
        Ok(sig.ret.map(|t| (Operand::Temp(dst.unwrap()), t)))
    }
}

/// 整数リテラル。`negative` なら `-n`。型の範囲に入らなければエラー。
fn int_literal(n: u64, negative: bool, ty: IntTy, span: Span) -> Result<Operand, Diag> {
    let v: i128 = if negative { -(n as i128) } else { n as i128 };
    if v < ir::min_of(ty) as i128 || v > ir::max_of(ty) as i128 {
        let shown = if negative {
            format!("-{}", n)
        } else {
            n.to_string()
        };
        return Err(Diag::new(
            span,
            format!(
                "`{}` は {} の範囲（{}〜{}）に入りません",
                shown,
                ty.name(),
                ir::min_of(ty),
                ir::max_of(ty)
            ),
        ));
    }
    Ok(Operand::Imm(v as i64))
}
