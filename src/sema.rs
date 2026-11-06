//! 型の確認と、中間表現への変換。
//!
//! 今できるのは手順 5 まで：
//!   整数（`8`・`32`・`64`）、`bool`、`address`、`string`、`array`、`pointer`、
//!   `maybe`（string・array・pointer に付けたもの）と `null`、`to:型(x)`、
//!   変数、`if`、`while`、関数呼び出し、グローバル変数、`import stdio;`。
//! それ以外の書き方は、構文としては読めても「まだ実装していません」のエラーにする。

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::fold;
use crate::ir::{self, CmpOp, ConstVal, Inst, IrBinOp, Operand, Place, Rt, Width};
use crate::source::{Diag, Span};

/// 値の型。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ty {
    Int(IntTy),
    Address,
    Bool,
    Str,
    Pointer(Box<Ty>),
    Array(Box<Ty>),
    Maybe(Box<Ty>),
    /// `null` そのもの。`maybe` の付いた型にだけ入れられる。
    Null,
}

const I32: Ty = Ty::Int(IntTy::I32);

impl Ty {
    fn name(&self) -> String {
        match self {
            Ty::Int(t) => t.name().to_string(),
            Ty::Address => "address".into(),
            Ty::Bool => "bool".into(),
            Ty::Str => "string".into(),
            Ty::Pointer(t) => format!("pointer:{}", t.name()),
            Ty::Array(t) => format!("array:{}", t.name()),
            Ty::Maybe(t) => format!("maybe:{}", t.name()),
            Ty::Null => "null".into(),
        }
    }

    /// 計算に使う整数の型。`address` は 64 として計算する。
    fn arith(&self) -> Option<IntTy> {
        match self {
            Ty::Int(t) => Some(*t),
            Ty::Address => Some(IntTy::I64),
            _ => None,
        }
    }

    /// メモリ上の幅（配列の要素や、ポインタの先）。
    fn width(&self) -> Width {
        match self {
            Ty::Int(IntTy::U8) | Ty::Bool => Width::W1,
            Ty::Int(IntTy::I32) => Width::W4,
            _ => Width::W8,
        }
    }

    fn is_maybe(&self) -> bool {
        matches!(self, Ty::Maybe(_))
    }

    /// 配列を表示するときの型の説明（ランタイムの orz_rt_put_array が読む）。
    fn desc(&self) -> String {
        match self {
            Ty::Int(IntTy::U8) => "b".into(),
            Ty::Int(IntTy::I32) => "i".into(),
            Ty::Int(IntTy::I64) => "l".into(),
            Ty::Bool => "B".into(),
            Ty::Str => "s".into(),
            Ty::Pointer(_) | Ty::Address | Ty::Null => "p".into(),
            Ty::Maybe(t) => format!("m{}", t.desc()),
            Ty::Array(t) => format!("[{}", t.desc()),
        }
    }
}

/// `from` の値を `to` 型の場所に入れられるか。
fn assignable(to: &Ty, from: &Ty) -> bool {
    to == from || matches!(to, Ty::Maybe(inner) if **inner == *from || *from == Ty::Null)
}

fn not_yet(span: Span, what: &str, step: u32) -> Diag {
    Diag::new(
        span,
        format!("{}はまだ実装していません（実装の手順 {} で対応）", what, step),
    )
}

/// ソースに書かれた型を確かめる。
fn resolve_type(t: &TypeExpr) -> Result<Ty, Diag> {
    Ok(match &t.kind {
        TypeKind::Int(it) => Ty::Int(*it),
        TypeKind::Bool => Ty::Bool,
        TypeKind::String => Ty::Str,
        TypeKind::Address => Ty::Address,
        TypeKind::Pointer(inner) => Ty::Pointer(Box::new(resolve_type(inner)?)),
        TypeKind::Array(inner) => Ty::Array(Box::new(resolve_type(inner)?)),
        TypeKind::Maybe(inner) => match resolve_type(inner)? {
            i @ (Ty::Str | Ty::Array(_) | Ty::Pointer(_)) => Ty::Maybe(Box::new(i)),
            Ty::Maybe(_) => return Err(Diag::new(t.span, "`maybe` は重ねて付けられません")),
            i => {
                return Err(not_yet(
                    t.span,
                    &format!("`maybe:{}`（整数などに付けた maybe）", i.name()),
                    6,
                ))
            }
        },
        TypeKind::Decnum => return Err(not_yet(t.span, "`decnum` 型", 6)),
        TypeKind::Named(..) => return Err(not_yet(t.span, "構造体", 6)),
        TypeKind::Function(..) => return Err(not_yet(t.span, "関数ポインタ", 7)),
    })
}

/// `import` なしで使える組み込みの関数。
const BUILTINS: &[&str] = &["allocate", "length", "size", "bytes", "copy", "collect"];

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
    Done(ConstVal),
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

/// 標準ライブラリ。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Module {
    Stdio,
}

struct Ctx<'p> {
    funcs: Vec<FuncSig>,
    globals: Vec<GlobalInfo<'p>>,
    names: HashMap<&'p str, TopName>,
    /// 名前空間の名前 → 標準ライブラリ
    modules: HashMap<String, Module>,
    strings: Vec<Vec<u8>>,
    string_ids: HashMap<Vec<u8>, u32>,
}

impl<'p> Ctx<'p> {
    fn intern(&mut self, s: &[u8]) -> u32 {
        if let Some(i) = self.string_ids.get(s) {
            return *i;
        }
        let i = self.strings.len() as u32;
        self.strings.push(s.to_vec());
        self.string_ids.insert(s.to_vec(), i);
        i
    }

    /// 名前が組み込みの関数・名前空間・関数・グローバル変数とぶつかるか。
    fn reserved(&self, span: Span, name: &str, what: &str) -> Result<(), Diag> {
        if BUILTINS.contains(&name) {
            return Err(Diag::new(
                span,
                format!("`{}` は組み込みの関数の名前なので、{}には使えません", name, what),
            ));
        }
        if self.modules.contains_key(name) {
            return Err(Diag::new(
                span,
                format!(
                    "`{}` は名前空間（import）の名前なので、{}には使えません",
                    name, what
                ),
            ));
        }
        Ok(())
    }
}

pub fn lower(prog: &Program) -> Result<ir::Program, Diag> {
    let mut ctx = Ctx {
        funcs: Vec::new(),
        globals: Vec::new(),
        names: HashMap::new(),
        modules: HashMap::new(),
        strings: Vec::new(),
        string_ids: HashMap::new(),
    };
    let mut func_asts: Vec<&Func> = Vec::new();

    // 1. import
    for item in &prog.items {
        let Item::Import(imp) = item else { continue };
        let (module, default_name) = match &imp.target {
            ImportTarget::Std(name) => match name.as_str() {
                "stdio" => (Module::Stdio, name.clone()),
                "math" | "socket" | "http" | "bignum" => {
                    return Err(Diag::new(imp.span, format!("標準ライブラリ `{}` はまだありません", name))
                        .hint("今使えるのは stdio だけです"))
                }
                _ => {
                    return Err(Diag::new(imp.span, format!("標準ライブラリに `{}` はありません", name))
                        .hint("自分のファイルなら `import \"ファイル名.orz\";` と書きます（今使える標準ライブラリは stdio だけです）"))
                }
            },
            ImportTarget::File(_) => {
                return Err(Diag::new(imp.span, "自分のファイルの `import` はまだ実装していません")
                    .hint("今使えるのは `import stdio;` だけです"))
            }
        };
        let ns = imp.alias.clone().unwrap_or(default_name);
        if ctx.modules.insert(ns.clone(), module).is_some() {
            return Err(Diag::new(
                imp.span,
                format!("名前空間 `{}` が 2 回 import されています", ns),
            ));
        }
    }

    // 2. 名前を集める。関数は定義より前の行からでも呼べる。
    for item in &prog.items {
        let (name, span, top) = match item {
            Item::Func(f) => (&f.name, f.name_span, TopName::Func(func_asts.len())),
            Item::Global(d) => (&d.name, d.name_span, TopName::Global(ctx.globals.len())),
            Item::Import(_) => continue,
        };
        let what = if matches!(top, TopName::Func(_)) {
            "関数の名前"
        } else {
            "グローバル変数の名前"
        };
        ctx.reserved(span, name, what)?;
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
            Item::Import(_) => {}
        }
    }

    // 3. 関数の引数と戻り値の型。
    for f in &func_asts {
        let mut seen = HashSet::new();
        let mut params = Vec::new();
        for p in &f.params {
            if !seen.insert(p.name.as_str()) {
                return Err(Diag::new(p.span, format!("引数 `{}` が 2 つあります", p.name)));
            }
            ctx.reserved(p.span, &p.name, "引数の名前")?;
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

    // 4. グローバル変数の初期値（コンパイル時に計算する）。
    for i in 0..ctx.globals.len() {
        ctx.eval_global(i)?;
    }

    // 5. main
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
    let (main_returns, main_takes_args) = check_main(func_asts[main], &ctx.funcs[main])?;

    // 6. 関数の中身。
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
        strings: ctx.strings,
        main: main as u32,
        main_returns,
        main_takes_args,
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

/// `main` の形を確かめる。（値を返すか、引数を受け取るか）
fn check_main(f: &Func, sig: &FuncSig) -> Result<(bool, bool), Diag> {
    let takes_args = match sig.params.as_slice() {
        [] => false,
        [Ty::Array(t)] if **t == Ty::Str => true,
        _ => {
            return Err(Diag::new(
                f.name_span,
                "`main` の引数は、なしか `args:array:string` だけです",
            )
            .hint("コマンドライン引数を受け取るなら `function nothing main(args:array:string)` と書きます"))
        }
    };
    match &sig.ret {
        None => Ok((false, takes_args)),
        Some(t) if *t == I32 => Ok((true, takes_args)),
        Some(_) => Err(Diag::new(
            f.ret.span(),
            "`main` の戻り値の型は `32` か `nothing` です",
        )),
    }
}

impl<'p> Ctx<'p> {
    /// グローバル変数の初期値を計算する。`immutable` の変数を使っていれば、先にそれを計算する。
    fn eval_global(&mut self, i: usize) -> Result<ConstVal, Diag> {
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
        let ty = self.globals[i].ty.clone();
        let mut l = FnLower::new(self, None, true);
        let op = l.expr_as(&decl.init, &ty)?;
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
    /// 今の場所で null でないとわかっている `maybe` の変数。
    narrowed: HashSet<u32>,
    /// 今の場所に来ることがあるか（`return` の後ろなどは来ない）。
    reachable: bool,
}

fn lower_func(ctx: &mut Ctx, index: usize, f: &Func) -> Result<ir::Function, Diag> {
    let sig = ctx.funcs[index].clone();
    let mut l = FnLower::new(ctx, sig.ret.clone(), false);
    collect_decls(&f.body, &mut l.declared_somewhere);
    l.scopes.push(Vec::new());
    for (p, ty) in f.params.iter().zip(&sig.params) {
        let var = l.new_var();
        l.scopes[0].push(Local {
            name: p.name.clone(),
            var,
            ty: ty.clone(),
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

/// ブロックの中で代入される変数の名前（`x = …`）。
fn assigned_names(b: &Block, out: &mut HashSet<String>) {
    for s in &b.stmts {
        assigned_names_stmt(s, out);
    }
}

fn assigned_names_stmt(s: &Stmt, out: &mut HashSet<String>) {
    match &s.kind {
        StmtKind::Assign {
            target: Expr {
                kind: ExprKind::Var(name),
                ..
            },
            ..
        } => {
            out.insert(name.clone());
        }
        StmtKind::If { then, els, .. } => {
            assigned_names(then, out);
            match els {
                Some(Else::Block(b)) => assigned_names(b, out),
                Some(Else::If(s)) => assigned_names_stmt(s, out),
                None => {}
            }
        }
        StmtKind::While { body, .. } | StmtKind::Arena(body) => assigned_names(body, out),
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

fn mismatch(span: Span, want: &Ty, got: &Ty) -> Diag {
    let d = Diag::new(
        span,
        format!(
            "`{}` 型の値が必要ですが、この式は `{}` 型です",
            want.name(),
            got.name()
        ),
    );
    match (want, got) {
        (w, Ty::Maybe(inner)) if **inner == *w => d.hint(
            "null かもしれない値は、`if (x != null) { … }` で確かめると、その中で使えます（グローバル変数は、いったんローカル変数に入れてから確かめます）",
        ),
        (_, Ty::Null) => d.hint("`null` は `maybe` の付いた型にだけ入れられます"),
        (Ty::Bool, Ty::Int(_)) => d.hint("整数を条件に使うなら、`x != 0` のように比べてください"),
        (Ty::Str, Ty::Int(_) | Ty::Bool | Ty::Address) => d.hint("文字列にするなら `to:string(…)` を使います"),
        (Ty::Int(a), Ty::Int(_)) => {
            d.hint(format!("幅の違う整数は混ぜられません。`to:{}(…)` で変換してください", a.name()))
        }
        _ => d,
    }
}

fn maybe_hint(d: Diag) -> Diag {
    d.hint("null かもしれない値は、`if (x != null) { … }` で確かめると、その中で使えます")
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
            narrowed: HashSet::new(),
            reachable: true,
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

    /// ランタイムの関数を呼ぶ。グローバル変数の初期値では呼べない。
    fn rt(&mut self, func: Rt, args: Vec<Operand>, span: Span, returns: bool) -> Result<Option<u32>, Diag> {
        if self.const_ctx {
            return Err(Diag::new(
                span,
                "グローバル変数の初期値には、値がコンパイル時に決まる式だけが書けます",
            )
            .hint("文字列の連結や配列づくりは、関数の中で行います"));
        }
        let dst = if returns { Some(self.temp()) } else { None };
        self.emit(Inst::CallRt {
            dst,
            func,
            args,
            span,
        });
        Ok(dst)
    }

    fn lookup(&self, name: &str) -> Option<&Local> {
        self.scopes
            .iter()
            .rev()
            .flat_map(|s| s.iter())
            .find(|l| l.name == name)
    }

    /// 変数の今の型（null でないとわかっていれば `maybe` を外す）。
    fn local_type(&self, l: &Local) -> Ty {
        match &l.ty {
            Ty::Maybe(inner) if self.narrowed.contains(&l.var) => (**inner).clone(),
            t => t.clone(),
        }
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
        self.ctx.reserved(span, name, "変数の名前")?;
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

    // ---------------- maybe の絞り込み ----------------

    /// `maybe` の型で宣言されたローカル変数なら、その番号。
    fn maybe_local(&self, e: &Expr) -> Option<u32> {
        match &e.kind {
            ExprKind::Var(name) => self.lookup(name).filter(|l| l.ty.is_maybe()).map(|l| l.var),
            _ => None,
        }
    }

    /// 条件が真のとき・偽のときに、null でないとわかる変数。
    fn cond_facts(&self, e: &Expr) -> (HashSet<u32>, HashSet<u32>) {
        match &e.kind {
            ExprKind::Binary(op @ (BinOp::Eq | BinOp::Ne), a, b) => {
                let var = match (&a.kind, &b.kind) {
                    (ExprKind::Null, _) => self.maybe_local(b),
                    (_, ExprKind::Null) => self.maybe_local(a),
                    _ => None,
                };
                let set: HashSet<u32> = var.into_iter().collect();
                if *op == BinOp::Ne {
                    (set, HashSet::new())
                } else {
                    (HashSet::new(), set)
                }
            }
            ExprKind::Binary(BinOp::And, a, b) => {
                let (ta, fa) = self.cond_facts(a);
                let (tb, fb) = self.cond_facts(b);
                (&ta | &tb, &fa & &fb)
            }
            ExprKind::Binary(BinOp::Or, a, b) => {
                let (ta, fa) = self.cond_facts(a);
                let (tb, fb) = self.cond_facts(b);
                (&ta & &tb, &fa | &fb)
            }
            ExprKind::Unary(UnOp::Not, x) => {
                let (t, f) = self.cond_facts(x);
                (f, t)
            }
            _ => (HashSet::new(), HashSet::new()),
        }
    }

    /// 変数に値を入れた後の絞り込み。null でない値を入れたら絞り込み、そうでなければ外す。
    fn after_store(&mut self, var: u32, declared: &Ty, value: &Ty) {
        if declared.is_maybe() {
            if value.is_maybe() || *value == Ty::Null {
                self.narrowed.remove(&var);
            } else {
                self.narrowed.insert(var);
            }
        }
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
                let (v, vt) = self.expr(&d.init, Some(&ty))?;
                if !assignable(&ty, &vt) {
                    return Err(mismatch(d.init.span, &ty, &vt));
                }
                let var = self.declare(&d.name, d.name_span, ty.clone())?;
                self.emit(Inst::Store {
                    place: Place::Var(var),
                    src: v,
                });
                self.after_store(var, &ty, &vt);
            }
            StmtKind::Assign { target, value } => self.assign(target, value)?,
            StmtKind::If { cond, then, els } => self.if_stmt(cond, then, els.as_ref())?,
            StmtKind::While { cond, body } => self.while_stmt(cond, body)?,
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
                self.reachable = false;
            }
            StmtKind::Return(None) => {
                if self.ret.is_some() {
                    return Err(Diag::new(s.span, "値を返す関数なので、`return` に値が必要です"));
                }
                self.emit(Inst::Ret(None));
                self.reachable = false;
            }
            StmtKind::Return(Some(e)) => {
                let Some(ty) = self.ret.clone() else {
                    return Err(Diag::new(e.span, "`nothing` の関数では値を `return` できません")
                        .hint("値を返すなら、`function 32 名前(...)` のように戻り値の型を書きます"));
                };
                let v = self.expr_as(e, &ty)?;
                self.emit(Inst::Ret(Some(v)));
                self.reachable = false;
            }
            StmtKind::Arena(_) => return Err(not_yet(s.span, "`arena`", 7)),
            StmtKind::Expr(e) => match &e.kind {
                ExprKind::Call(callee, args) => {
                    self.call(e, callee, args, None)?;
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
        match &target.kind {
            ExprKind::Var(name) => {
                let (place, ty, local) = if let Some(l) = self.lookup(name) {
                    (Place::Var(l.var), l.ty.clone(), Some(l.var))
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
                            (Place::Global(*i as u32), g.ty.clone(), None)
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
                let (v, vt) = self.expr(value, Some(&ty))?;
                if !assignable(&ty, &vt) {
                    return Err(mismatch(value.span, &ty, &vt));
                }
                self.emit(Inst::Store { place, src: v });
                if let Some(var) = local {
                    self.after_store(var, &ty, &vt);
                }
                Ok(())
            }
            ExprKind::Index(base, index) => {
                let (addr, elem) = self.elem_addr(target, base, index)?;
                let v = self.expr_as(value, &elem)?;
                self.emit(Inst::StoreMem {
                    addr,
                    src: v,
                    width: elem.width(),
                });
                Ok(())
            }
            ExprKind::Field(..) => Err(not_yet(target.span, "フィールドへの代入", 6)),
            _ => Err(Diag::new(target.span, "ここには代入できません")
                .hint("`=` の左には変数か `a[i]` を書きます。比べるなら `==` です")),
        }
    }

    fn if_stmt(&mut self, cond: &Expr, then: &Block, els: Option<&Else>) -> Result<(), Diag> {
        let (true_facts, false_facts) = self.cond_facts(cond);
        let entry = self.narrowed.clone();
        let entry_reachable = self.reachable;
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
        self.narrowed = &entry | &true_facts;
        self.reachable = entry_reachable;
        self.block(then)?;
        let then_flow = (self.reachable, std::mem::take(&mut self.narrowed));
        self.emit(Inst::Jump(end));

        self.emit(Inst::Label(else_l));
        self.narrowed = &entry | &false_facts;
        self.reachable = entry_reachable;
        match els {
            Some(Else::Block(b)) => self.block(b)?,
            Some(Else::If(s)) => self.stmt(s)?,
            None => {}
        }
        let else_flow = (self.reachable, std::mem::take(&mut self.narrowed));
        self.emit(Inst::Label(end));

        // 合流：来ることのある道だけで、共通して null でないもの。
        (self.reachable, self.narrowed) = match (then_flow, else_flow) {
            ((true, t), (true, e)) => (true, &t & &e),
            ((true, t), (false, _)) => (true, t),
            ((false, _), (true, e)) => (true, e),
            ((false, t), (false, _)) => (false, t),
        };
        Ok(())
    }

    fn while_stmt(&mut self, cond: &Expr, body: &Block) -> Result<(), Diag> {
        // ループの中で代入される変数は、先頭では null でないとは言えない。
        let mut assigned = HashSet::new();
        assigned_names(body, &mut assigned);
        let assigned_vars: HashSet<u32> = assigned
            .iter()
            .filter_map(|n| self.lookup(n).map(|l| l.var))
            .collect();
        let head_state: HashSet<u32> = &self.narrowed - &assigned_vars;
        let entry_reachable = self.reachable;
        let (true_facts, false_facts) = self.cond_facts(cond);

        let head = self.label();
        let body_l = self.label();
        let end = self.label();
        self.emit(Inst::Label(head));
        self.narrowed = head_state.clone();
        let c = self.cond(cond)?;
        self.emit(Inst::Branch {
            cond: c,
            then: body_l,
            els: end,
        });
        self.emit(Inst::Label(body_l));
        self.narrowed = &head_state | &true_facts;
        self.loops.push((head, end));
        self.block(body)?;
        self.loops.pop();
        self.emit(Inst::Jump(head));
        self.emit(Inst::Label(end));

        let breaks = has_break(body);
        self.narrowed = if breaks {
            head_state
        } else {
            &head_state | &false_facts
        };
        let infinite = matches!(cond.kind, ExprKind::Bool(true)) && !breaks;
        self.reachable = entry_reachable && !infinite;
        Ok(())
    }

    /// `if` や `while` の条件。`bool` だけ。
    fn cond(&mut self, e: &Expr) -> Result<Operand, Diag> {
        let (v, t) = self.expr(e, Some(&Ty::Bool))?;
        if t != Ty::Bool {
            return Err(Diag::new(
                e.span,
                format!(
                    "条件に書けるのは `bool` の値だけですが、この式は `{}` 型です",
                    t.name()
                ),
            )
            .hint("整数なら `x != 0`、maybe なら `x != null` のように比べてください"));
        }
        Ok(v)
    }

    // ---------------- 式 ----------------

    /// 式を `ty` 型の値として計算する。入れられない型ならエラー。
    fn expr_as(&mut self, e: &Expr, ty: &Ty) -> Result<Operand, Diag> {
        let (v, t) = self.expr(e, Some(ty))?;
        if !assignable(ty, &t) {
            return Err(mismatch(e.span, ty, &t));
        }
        Ok(v)
    }

    /// 式の型を、計算せずに調べる。整数リテラルだけの式など、文脈で決まるものは `None`。
    fn peek(&self, e: &Expr) -> Option<Ty> {
        match &e.kind {
            ExprKind::Bool(_) => Some(Ty::Bool),
            ExprKind::Str(_) => Some(Ty::Str),
            ExprKind::Char(_) => Some(Ty::Int(IntTy::U8)),
            ExprKind::Null => Some(Ty::Null),
            ExprKind::Var(name) => {
                if let Some(l) = self.lookup(name) {
                    return Some(self.local_type(l));
                }
                match self.ctx.names.get(name.as_str()) {
                    Some(TopName::Global(i)) => Some(self.ctx.globals[*i].ty.clone()),
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
                ExprKind::Var(name) if self.lookup(name).is_none() => match name.as_str() {
                    "length" => Some(I32),
                    "bytes" => Some(Ty::Pointer(Box::new(Ty::Int(IntTy::U8)))),
                    _ => match self.ctx.names.get(name.as_str()) {
                        Some(TopName::Func(i)) => self.ctx.funcs[*i].ret.clone(),
                        _ => None,
                    },
                },
                ExprKind::Field(obj, f) => match &obj.kind {
                    ExprKind::Var(ns) if self.ctx.modules.get(ns) == Some(&Module::Stdio) => match f.as_str()
                    {
                        "read_line" | "read_file" => Some(Ty::Maybe(Box::new(Ty::Str))),
                        "write_file" => Some(Ty::Bool),
                        "last_error" => Some(Ty::Str),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            },
            ExprKind::Index(base, _) => match self.peek(base)? {
                Ty::Array(t) | Ty::Pointer(t) => Some(*t),
                _ => None,
            },
            ExprKind::Convert(t, _) => resolve_type(t).ok(),
            ExprKind::ArrayLit(elems) => {
                let t = elems.first().and_then(|x| self.peek(x))?;
                Some(Ty::Array(Box::new(t)))
            }
            _ => None,
        }
    }

    /// 式を計算する。`want` は文脈から期待される型（整数リテラルの型などを決めるのに使う）。
    fn expr(&mut self, e: &Expr, want: Option<&Ty>) -> Result<(Operand, Ty), Diag> {
        match &e.kind {
            ExprKind::Int(n) => int_literal(*n, false, want, e.span),
            ExprKind::Char(c) => Ok((Operand::Imm(*c as i64), Ty::Int(IntTy::U8))),
            ExprKind::Bool(b) => Ok((Operand::Imm(*b as i64), Ty::Bool)),
            ExprKind::Str(s) => Ok((Operand::Str(self.ctx.intern(s)), Ty::Str)),
            ExprKind::Null => Ok((Operand::Imm(0), Ty::Null)),
            ExprKind::Var(name) => self.var(e.span, name),
            ExprKind::Unary(UnOp::Neg, inner) => {
                // `-2147483648` は、2147483648 が 32 に入らなくても書けるようにする。
                if let ExprKind::Int(n) = inner.kind {
                    return int_literal(n, true, want, e.span);
                }
                let (a, t) = self.expr(inner, want)?;
                let Some(it) = t.arith() else {
                    let d = Diag::new(e.span, format!("符号の `-` は `{}` には使えません", t.name()));
                    return Err(if t == Ty::Bool {
                        d.hint("真と偽を入れ替えるなら `not` を使います")
                    } else {
                        d
                    });
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
                match (&t, t.arith()) {
                    (Ty::Bool, _) => self.emit(Inst::BoolNot { dst, a }),
                    (_, Some(it)) => self.emit(Inst::Not { dst, ty: it, a }),
                    _ => {
                        return Err(Diag::new(
                            e.span,
                            format!(
                                "`not` は `bool` と整数にだけ使えますが、この式は `{}` 型です",
                                t.name()
                            ),
                        ))
                    }
                }
                Ok((Operand::Temp(dst), t))
            }
            ExprKind::Binary(op, a, b) => self.binary(e, *op, a, b, want),
            ExprKind::Call(callee, args) => match self.call(e, callee, args, want)? {
                Some(v) => Ok(v),
                None => {
                    let name = match &callee.kind {
                        ExprKind::Var(n) => n.clone(),
                        ExprKind::Field(_, f) => f.clone(),
                        _ => "?".into(),
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
            ExprKind::Index(base, index) => {
                let (addr, elem) = self.elem_addr(e, base, index)?;
                let dst = self.temp();
                self.emit(Inst::Load {
                    dst,
                    addr,
                    width: elem.width(),
                });
                Ok((Operand::Temp(dst), elem))
            }
            ExprKind::Convert(ty, x) => self.convert(e, ty, x),
            ExprKind::SizeOf(ty) => {
                let t = resolve_type(ty)?;
                int_literal(t.width().bytes() as u64, false, want, e.span)
            }
            ExprKind::ArrayLit(elems) => self.array_lit(e, elems, want),
            ExprKind::ArrayNew(n) => self.array_new(e, n, want),
            ExprKind::Field(obj, _) => match &obj.kind {
                ExprKind::Var(ns) if self.ctx.modules.contains_key(ns) => Err(Diag::new(
                    e.span,
                    "名前空間の関数は、`stdio.print(…)` のように呼び出しの形で使います",
                )),
                _ => Err(not_yet(e.span, "フィールド（`.`）", 6)),
            },
            ExprKind::Float(_) => Err(not_yet(e.span, "小数（`decnum` 型）", 6)),
        }
    }

    fn var(&mut self, span: Span, name: &str) -> Result<(Operand, Ty), Diag> {
        if let Some(l) = self.lookup(name) {
            let (var, ty) = (l.var, self.local_type(l));
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
                let (ty, immutable) = (self.ctx.globals[i].ty.clone(), self.ctx.globals[i].decl.immutable);
                if immutable {
                    // immutable の値はコンパイル時に決まっているので、そのまま埋め込む。
                    let v = match self.ctx.eval_global(i)? {
                        ConstVal::Int(v) => Operand::Imm(v),
                        ConstVal::Str(s) => Operand::Str(s),
                    };
                    return Ok((v, ty));
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
            None if self.ctx.modules.contains_key(name) => Err(Diag::new(
                span,
                format!("`{}` は名前空間です。`{}.print(…)` のように使います", name, name),
            )),
            None if BUILTINS.contains(&name) => Err(Diag::new(
                span,
                format!(
                    "`{}` は組み込みの関数です。`{}(…)` のように呼び出します",
                    name, name
                ),
            )),
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

    /// `a[i]` の要素のアドレス。（アドレス, 要素の型）
    fn elem_addr(&mut self, e: &Expr, base: &Expr, index: &Expr) -> Result<(Operand, Ty), Diag> {
        let (bv, bt) = self.expr(base, None)?;
        let (elem, checked) = match &bt {
            Ty::Array(t) => ((**t).clone(), true),
            Ty::Pointer(t) => ((**t).clone(), false),
            Ty::Str => {
                return Err(Diag::new(e.span, "文字列に `[ ]` は使えません").hint(
                    "バイトとして読むなら `bytes(s)[i]` と書きます（UTF-8 の 1 文字は 1〜4 バイトです）",
                ))
            }
            Ty::Maybe(_) => {
                return Err(maybe_hint(Diag::new(
                    base.span,
                    format!(
                        "`{}` 型の値には、null でないことを確かめるまで `[ ]` は使えません",
                        bt.name()
                    ),
                )))
            }
            _ => {
                return Err(Diag::new(
                    base.span,
                    format!(
                        "`[ ]` は array と pointer にだけ使えますが、この式は `{}` 型です",
                        bt.name()
                    ),
                ))
            }
        };
        let (iv, it) = self.expr(index, None)?;
        if !matches!(it, Ty::Int(_)) {
            return Err(Diag::new(
                index.span,
                format!("添字は整数ですが、この式は `{}` 型です", it.name()),
            ));
        }
        if checked {
            self.emit(Inst::CheckIndex {
                array: bv,
                index: iv,
                span: e.span,
            });
        }
        let dst = self.temp();
        self.emit(Inst::ElemAddr {
            dst,
            base: bv,
            index: iv,
            scale: elem.width().bytes(),
        });
        Ok((Operand::Temp(dst), elem))
    }

    fn binary(
        &mut self,
        e: &Expr,
        op: BinOp,
        a: &Expr,
        b: &Expr,
        want: Option<&Ty>,
    ) -> Result<(Operand, Ty), Diag> {
        if is_comparison(op) {
            return self.comparison(e, op, a, b);
        }
        let want_num = want.filter(|w| w.arith().is_some()).cloned();
        let t = if is_logic(op) {
            self.peek(a)
                .or_else(|| self.peek(b))
                .or(want.cloned())
                .unwrap_or(I32)
        } else {
            self.peek(a).or_else(|| self.peek(b)).or(want_num).unwrap_or(I32)
        };
        if op == BinOp::Add && t == Ty::Str {
            let av = self.expr_as(a, &Ty::Str)?;
            let bv = self.expr_as(b, &Ty::Str)?;
            let dst = self.rt(Rt::Concat, vec![av, bv], e.span, true)?.unwrap();
            return Ok((Operand::Temp(dst), Ty::Str));
        }
        if t == Ty::Bool && is_logic(op) {
            return self.logic_bool(op, a, b);
        }
        let Some(it) = t.arith() else {
            let d = Diag::new(
                e.span,
                format!("`{}` は `{}` には使えません", op.as_str(), t.name()),
            );
            return Err(match t {
                Ty::Bool => d.hint("真偽の演算には `and`・`or`・`not` などを使います"),
                Ty::Str => d.hint("文字列でできるのは `+`（連結）と `==`・`!=` だけです"),
                _ => d,
            });
        };
        let av = self.expr_as(a, &t)?;
        let bv = self.expr_as(b, &t)?;
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
            _ => unreachable!("比較は別に処理する"),
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

    fn comparison(&mut self, e: &Expr, op: BinOp, a: &Expr, b: &Expr) -> Result<(Operand, Ty), Diag> {
        let cop = match op {
            BinOp::Lt => CmpOp::Lt,
            BinOp::Gt => CmpOp::Gt,
            BinOp::Le => CmpOp::Le,
            BinOp::Ge => CmpOp::Ge,
            BinOp::Eq => CmpOp::Eq,
            _ => CmpOp::Ne,
        };
        let a_null = matches!(a.kind, ExprKind::Null);
        let b_null = matches!(b.kind, ExprKind::Null);
        if a_null || b_null {
            if !matches!(op, BinOp::Eq | BinOp::Ne) {
                return Err(Diag::new(e.span, "`null` と比べられるのは `==` と `!=` だけです"));
            }
            if a_null && b_null {
                return Err(Diag::new(e.span, "`null` どうしを比べています"));
            }
            let other = if a_null { b } else { a };
            let (ov, ot) = self.expr(other, None)?;
            let declared_maybe = self.maybe_local(other).is_some();
            if !ot.is_maybe() && !declared_maybe {
                return Err(Diag::new(
                    other.span,
                    format!(
                        "`{}` 型の値は null になりません（maybe ではありません）",
                        ot.name()
                    ),
                )
                .hint("null と比べられるのは、maybe の付いた型の値だけです"));
            }
            let dst = self.temp();
            self.emit(Inst::Cmp {
                dst,
                op: cop,
                a: ov,
                b: Operand::Imm(0),
            });
            return Ok((Operand::Temp(dst), Ty::Bool));
        }

        let pa = self.peek(a);
        let pb = self.peek(b);
        // maybe:string と string のように、片方が他方に入る型なら、広い方で比べる。
        let t = match (&pa, &pb) {
            (Some(x), Some(y)) if x != y && assignable(x, y) => x.clone(),
            (Some(x), Some(y)) if x != y && assignable(y, x) => y.clone(),
            _ => pa.or(pb).unwrap_or(I32),
        };
        let av = self.expr_as(a, &t)?;
        let bv = self.expr_as(b, &t)?;
        let dst = self.temp();
        match op {
            BinOp::Eq | BinOp::Ne => {
                let base = match &t {
                    Ty::Maybe(inner) => &**inner,
                    t => t,
                };
                if *base == Ty::Str {
                    // 文字列は中身を比べる。
                    let eq = self.rt(Rt::StrEq, vec![av, bv], e.span, true)?.unwrap();
                    if op == BinOp::Eq {
                        return Ok((Operand::Temp(eq), Ty::Bool));
                    }
                    self.emit(Inst::BoolNot {
                        dst,
                        a: Operand::Temp(eq),
                    });
                } else {
                    // array と pointer は、同じものを指しているかを比べる。
                    self.emit(Inst::Cmp {
                        dst,
                        op: cop,
                        a: av,
                        b: bv,
                    });
                }
            }
            _ => {
                if t.arith().is_none() {
                    return Err(Diag::new(
                        e.span,
                        format!(
                            "`{}` で大小を比べられるのは整数だけですが、この式は `{}` 型です",
                            op.as_str(),
                            t.name()
                        ),
                    ));
                }
                self.emit(Inst::Cmp {
                    dst,
                    op: cop,
                    a: av,
                    b: bv,
                });
            }
        }
        Ok((Operand::Temp(dst), Ty::Bool))
    }

    /// `bool` の `and`・`or` など。`and`・`nand`・`or`・`nor` は、左辺で結果が決まれば右辺を計算しない。
    fn logic_bool(&mut self, op: BinOp, a: &Expr, b: &Expr) -> Result<(Operand, Ty), Diag> {
        let av = self.expr_as(a, &Ty::Bool)?;
        if matches!(op, BinOp::Xor | BinOp::Xnor) {
            let bv = self.expr_as(b, &Ty::Bool)?;
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
        let is_and = matches!(op, BinOp::And | BinOp::Nand);
        if self.const_ctx {
            // 初期値の計算では副作用がないので、両辺を計算してよい。
            let bv = self.expr_as(b, &Ty::Bool)?;
            let dst = self.temp();
            let irop = if is_and { IrBinOp::And } else { IrBinOp::Or };
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
        // 右辺は「左辺が真（and）・偽（or）」のときだけ計算するので、そのぶん絞り込める。
        let saved = self.narrowed.clone();
        let (ta, fa) = self.cond_facts(a);
        self.narrowed.extend(if is_and { ta } else { fa });
        let bv = self.expr_as(b, &Ty::Bool);
        self.narrowed = saved;
        let mut bv = bv?;
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
    fn call(
        &mut self,
        e: &Expr,
        callee: &Expr,
        args: &[Expr],
        want: Option<&Ty>,
    ) -> Result<Option<(Operand, Ty)>, Diag> {
        let name = match &callee.kind {
            ExprKind::Var(n) => n,
            ExprKind::Field(obj, f) => {
                if let ExprKind::Var(ns) = &obj.kind {
                    if let Some(m) = self.ctx.modules.get(ns).copied() {
                        return self.module_call(e, m, ns, f, args);
                    }
                }
                return Err(not_yet(callee.span, "フィールドに入った関数の呼び出し", 7));
            }
            _ => return Err(not_yet(callee.span, "この形の関数呼び出し", 7)),
        };
        if self.lookup(name).is_some() {
            return Err(not_yet(
                callee.span,
                "変数に入った関数の呼び出し（関数ポインタ）",
                7,
            ));
        }
        if BUILTINS.contains(&name.as_str()) {
            return self.builtin_call(e, name, args, want);
        }
        if self.const_ctx {
            return Err(Diag::new(e.span, "グローバル変数の初期値では、関数を呼べません")
                .hint("初期値には、値がコンパイル時に決まる式だけが書けます"));
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
                    d.hint(format!(
                        "表示は `import stdio;` を書いて、`stdio.{}(…)` で行います",
                        name
                    ))
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
            ops.push(self.expr_as(arg, ty)?);
        }
        let dst = sig.ret.as_ref().map(|_| self.temp());
        self.emit(Inst::Call {
            dst,
            func: index as u32,
            args: ops,
        });
        Ok(sig.ret.map(|t| (Operand::Temp(dst.unwrap()), t)))
    }

    fn arg_count(&self, e: &Expr, name: &str, args: &[Expr], n: usize) -> Result<(), Diag> {
        if args.len() != n {
            return Err(Diag::new(
                e.span,
                format!(
                    "`{}` の引数は {} 個ですが、{} 個書かれています",
                    name,
                    n,
                    args.len()
                ),
            ));
        }
        Ok(())
    }

    /// `import` なしで使える関数。
    fn builtin_call(
        &mut self,
        e: &Expr,
        name: &str,
        args: &[Expr],
        want: Option<&Ty>,
    ) -> Result<Option<(Operand, Ty)>, Diag> {
        match name {
            "length" => {
                self.arg_count(e, name, args, 1)?;
                let (v, t) = self.expr(&args[0], None)?;
                match &t {
                    Ty::Array(_) | Ty::Str => {}
                    Ty::Maybe(_) => {
                        return Err(maybe_hint(Diag::new(
                            args[0].span,
                            format!(
                                "`{}` 型の値には、null でないことを確かめるまで `length` は使えません",
                                t.name()
                            ),
                        )))
                    }
                    Ty::Pointer(_) => {
                        return Err(Diag::new(
                            args[0].span,
                            "pointer は長さを持っていないので、`length` は使えません",
                        )
                        .hint("長さがほしいなら array を使います"))
                    }
                    _ => {
                        return Err(Diag::new(
                            args[0].span,
                            format!(
                                "`length` は array と string にだけ使えますが、この式は `{}` 型です",
                                t.name()
                            ),
                        ))
                    }
                }
                if self.const_ctx {
                    return Err(Diag::new(
                        e.span,
                        "グローバル変数の初期値には、値がコンパイル時に決まる式だけが書けます",
                    ));
                }
                let dst = self.temp();
                self.emit(Inst::Length { dst, a: v });
                Ok(Some((Operand::Temp(dst), I32)))
            }
            "allocate" => {
                self.arg_count(e, name, args, 1)?;
                let ptr = match want {
                    Some(Ty::Pointer(t)) => Ty::Pointer(t.clone()),
                    Some(Ty::Maybe(inner)) if matches!(**inner, Ty::Pointer(_)) => (**inner).clone(),
                    _ => {
                        return Err(
                            Diag::new(e.span, "`allocate` の結果は、pointer 型の場所に入れます")
                                .hint("例：p:pointer:32 = allocate(3 * size(:32));"),
                        )
                    }
                };
                let (n, nt) = self.expr(&args[0], Some(&I32))?;
                if nt.arith().is_none() || nt == Ty::Address {
                    return Err(Diag::new(
                        args[0].span,
                        format!("大きさは整数ですが、この式は `{}` 型です", nt.name()),
                    ));
                }
                let dst = self.rt(Rt::Alloc, vec![n], e.span, true)?.unwrap();
                Ok(Some((Operand::Temp(dst), ptr)))
            }
            "bytes" => {
                self.arg_count(e, name, args, 1)?;
                let (v, t) = self.expr(&args[0], None)?;
                match t {
                    Ty::Str | Ty::Address => Ok(Some((v, Ty::Pointer(Box::new(Ty::Int(IntTy::U8)))))),
                    Ty::Maybe(_) => Err(maybe_hint(Diag::new(
                        args[0].span,
                        format!(
                            "`{}` 型の値には、null でないことを確かめるまで `bytes` は使えません",
                            t.name()
                        ),
                    ))),
                    _ => Err(Diag::new(
                        args[0].span,
                        format!(
                            "`bytes` は string と address にだけ使えますが、この式は `{}` 型です",
                            t.name()
                        ),
                    )),
                }
            }
            "size" => Err(Diag::new(e.span, "`size` には型を書きます").hint("例：size(:32)")),
            "copy" => Err(not_yet(e.span, "`copy`（構造体の複製）", 6)),
            "collect" => Err(not_yet(e.span, "`collect`（ガベージコレクション）", 7)),
            _ => unreachable!("組み込みの関数は上ですべて扱う"),
        }
    }

    /// `stdio.print(…)` など。
    fn module_call(
        &mut self,
        e: &Expr,
        module: Module,
        ns: &str,
        name: &str,
        args: &[Expr],
    ) -> Result<Option<(Operand, Ty)>, Diag> {
        let Module::Stdio = module;
        let full = format!("{}.{}", ns, name);
        let maybe_str = Ty::Maybe(Box::new(Ty::Str));
        match name {
            "print" | "put" => {
                if args.len() != 1 {
                    return Err(Diag::new(
                        e.span,
                        format!(
                            "`{}` に渡せる値は 1 つですが、{} 個書かれています",
                            full,
                            args.len()
                        ),
                    )
                    .hint("文字列なら `+` でつなげるか、`put` を何回かに分けて使います"));
                }
                let (v, t) = self.expr(&args[0], None)?;
                let span = e.span;
                match &t {
                    Ty::Int(_) => self.rt(Rt::PutInt, vec![v], span, false)?,
                    Ty::Address | Ty::Pointer(_) => self.rt(Rt::PutAddr, vec![v], span, false)?,
                    Ty::Maybe(inner) if matches!(**inner, Ty::Pointer(_)) => {
                        self.rt(Rt::PutAddr, vec![v], span, false)?
                    }
                    Ty::Bool => self.rt(Rt::PutBool, vec![v], span, false)?,
                    Ty::Str | Ty::Null => self.rt(Rt::PutStr, vec![v], span, false)?,
                    Ty::Maybe(inner) if **inner == Ty::Str => self.rt(Rt::PutStr, vec![v], span, false)?,
                    Ty::Array(_) | Ty::Maybe(_) => {
                        let elem = match &t {
                            Ty::Array(x) => x,
                            Ty::Maybe(inner) => match &**inner {
                                Ty::Array(x) => x,
                                _ => unreachable!("maybe は string・array・pointer にだけ付く"),
                            },
                            _ => unreachable!(),
                        };
                        let desc = self.ctx.intern(elem.desc().as_bytes());
                        self.rt(Rt::PutArray, vec![v, Operand::Str(desc)], span, false)?
                    }
                };
                if name == "print" {
                    self.rt(Rt::Newline, vec![], span, false)?;
                }
                Ok(None)
            }
            "read_line" => {
                self.arg_count(e, &full, args, 0)?;
                let dst = self.rt(Rt::ReadLine, vec![], e.span, true)?.unwrap();
                Ok(Some((Operand::Temp(dst), maybe_str)))
            }
            "read_file" => {
                self.arg_count(e, &full, args, 1)?;
                let path = self.expr_as(&args[0], &Ty::Str)?;
                let dst = self.rt(Rt::ReadFile, vec![path], e.span, true)?.unwrap();
                Ok(Some((Operand::Temp(dst), maybe_str)))
            }
            "write_file" => {
                self.arg_count(e, &full, args, 2)?;
                let path = self.expr_as(&args[0], &Ty::Str)?;
                let text = self.expr_as(&args[1], &Ty::Str)?;
                let dst = self.rt(Rt::WriteFile, vec![path, text], e.span, true)?.unwrap();
                Ok(Some((Operand::Temp(dst), Ty::Bool)))
            }
            "last_error" => {
                self.arg_count(e, &full, args, 0)?;
                let dst = self.rt(Rt::LastError, vec![], e.span, true)?.unwrap();
                Ok(Some((Operand::Temp(dst), Ty::Str)))
            }
            _ => Err(Diag::new(e.span, format!("`{}` に `{}` はありません", ns, name))
                .hint("stdio にあるのは print、put、read_line、read_file、write_file、last_error です")),
        }
    }

    /// `to:型(x)`
    fn convert(&mut self, e: &Expr, ty: &TypeExpr, x: &Expr) -> Result<(Operand, Ty), Diag> {
        let to = resolve_type(ty)?;
        if to == Ty::Str {
            let (v, from) = self.expr(x, None)?;
            let func = match &from {
                Ty::Str => return Ok((v, Ty::Str)),
                Ty::Int(_) | Ty::Address => Rt::StrFromInt,
                Ty::Bool => Rt::StrFromBool,
                Ty::Maybe(_) => {
                    return Err(maybe_hint(Diag::new(
                        x.span,
                        format!(
                            "`{}` 型の値は、null でないことを確かめてから変換します",
                            from.name()
                        ),
                    )))
                }
                _ => {
                    return Err(Diag::new(
                        e.span,
                        format!("`{}` は `string` に変換できません", from.name()),
                    ))
                }
            };
            let dst = self.rt(func, vec![v], e.span, true)?.unwrap();
            return Ok((Operand::Temp(dst), Ty::Str));
        }
        // 整数リテラルは、変換先の型として読む（`to:64(3000000000)` を書けるように）。
        let want = if to.arith().is_some() {
            Some(to.clone())
        } else {
            None
        };
        let (v, from) = self.expr(x, want.as_ref())?;
        if from == to {
            return Ok((v, to));
        }
        match (&from, &to) {
            (Ty::Int(f), Ty::Int(t)) => {
                let dst = self.temp();
                self.emit(Inst::Convert {
                    dst,
                    from: *f,
                    to: *t,
                    a: v,
                    span: e.span,
                });
                Ok((Operand::Temp(dst), to))
            }
            // 同じ 8 バイトの値なので、そのまま使う。
            (Ty::Int(IntTy::I64), Ty::Address)
            | (Ty::Address, Ty::Int(IntTy::I64))
            | (Ty::Str | Ty::Array(_) | Ty::Pointer(_), Ty::Address)
            | (Ty::Address, Ty::Pointer(_))
            | (Ty::Bool, Ty::Int(_)) => Ok((v, to)),
            (Ty::Str, Ty::Int(_)) => Err(not_yet(e.span, "文字列から数への変換（結果は maybe:32 など）", 6)),
            (Ty::Maybe(_), _) => Err(maybe_hint(Diag::new(
                x.span,
                format!(
                    "`{}` 型の値は、null でないことを確かめてから変換します",
                    from.name()
                ),
            ))),
            (Ty::Int(_), Ty::Address) => Err(Diag::new(e.span, "`address` に変換できる整数は `64` だけです")
                .hint("先に `to:64(…)` で 64 にしてください")),
            _ => Err(Diag::new(
                e.span,
                format!("`{}` は `{}` に変換できません", from.name(), to.name()),
            )),
        }
    }

    /// `[1, 2, 3]`
    fn array_lit(&mut self, e: &Expr, elems: &[Expr], want: Option<&Ty>) -> Result<(Operand, Ty), Diag> {
        let elem = match want {
            Some(Ty::Array(t)) => (**t).clone(),
            Some(Ty::Maybe(inner)) if matches!(**inner, Ty::Array(_)) => match &**inner {
                Ty::Array(t) => (**t).clone(),
                _ => unreachable!(),
            },
            _ => match elems.first().and_then(|x| self.peek(x)) {
                Some(Ty::Null) | None if elems.is_empty() => {
                    return Err(
                        Diag::new(e.span, "空の配列 `[]` は、型のわかる場所でだけ使えます")
                            .hint("例：a:array:32 = [];"),
                    )
                }
                Some(Ty::Null) => {
                    return Err(Diag::new(e.span, "要素の型がわかりません")
                        .hint("例：a:array:maybe:string = [null];"))
                }
                Some(t) => t,
                None => I32,
            },
        };
        let width = elem.width();
        let arr = self
            .rt(
                Rt::ArrayNew,
                vec![
                    Operand::Imm(elems.len() as i64),
                    Operand::Imm(width.bytes()),
                    Operand::Imm(0),
                ],
                e.span,
                true,
            )?
            .unwrap();
        for (i, el) in elems.iter().enumerate() {
            let v = self.expr_as(el, &elem)?;
            let addr = self.temp();
            self.emit(Inst::ElemAddr {
                dst: addr,
                base: Operand::Temp(arr),
                index: Operand::Imm(i as i64),
                scale: width.bytes(),
            });
            self.emit(Inst::StoreMem {
                addr: Operand::Temp(addr),
                src: v,
                width,
            });
        }
        Ok((Operand::Temp(arr), Ty::Array(Box::new(elem))))
    }

    /// `array(n)`
    fn array_new(&mut self, e: &Expr, n: &Expr, want: Option<&Ty>) -> Result<(Operand, Ty), Diag> {
        let elem = match want {
            Some(Ty::Array(t)) => (**t).clone(),
            Some(Ty::Maybe(inner)) if matches!(**inner, Ty::Array(_)) => match &**inner {
                Ty::Array(t) => (**t).clone(),
                _ => unreachable!(),
            },
            _ => {
                return Err(Diag::new(e.span, "`array(n)` は、型のわかる場所でだけ使えます")
                    .hint("例：a:array:32 = array(10);"))
            }
        };
        let (nv, nt) = self.expr(n, Some(&I32))?;
        if !matches!(nt, Ty::Int(_)) {
            return Err(Diag::new(
                n.span,
                format!("要素の数は整数ですが、この式は `{}` 型です", nt.name()),
            ));
        }
        // 初期値：整数などは 0、string は ""、array は空の配列（null にはしない）。
        let kind = match elem {
            Ty::Str => 1,
            Ty::Array(_) => 2,
            _ => 0,
        };
        let arr = self
            .rt(
                Rt::ArrayNew,
                vec![nv, Operand::Imm(elem.width().bytes()), Operand::Imm(kind)],
                e.span,
                true,
            )?
            .unwrap();
        Ok((Operand::Temp(arr), Ty::Array(Box::new(elem))))
    }
}

/// 整数リテラル。`negative` なら `-n`。型は文脈から決め、決まらなければ `32`。
fn int_literal(n: u64, negative: bool, want: Option<&Ty>, span: Span) -> Result<(Operand, Ty), Diag> {
    let ty = match want {
        Some(t @ (Ty::Int(_) | Ty::Address)) => t.clone(),
        _ => I32,
    };
    let it = ty.arith().unwrap();
    let v: i128 = if negative { -(n as i128) } else { n as i128 };
    if v < ir::min_of(it) as i128 || v > ir::max_of(it) as i128 {
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
                ir::min_of(it),
                ir::max_of(it)
            ),
        ));
    }
    Ok((Operand::Imm(v as i64), ty))
}
