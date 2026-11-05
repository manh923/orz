//! 型の確認と、中間表現への変換。
//!
//! 今できるのは手順 3 まで：`32` の整数式と `return`。
//! それ以外の書き方は、構文としては読めても「まだ実装していません」のエラーにする。

use std::collections::HashMap;

use crate::ast::*;
use crate::ir::{self, Inst, IrBinOp, Operand};
use crate::source::{Diag, Span};

pub fn lower(prog: &Program) -> Result<ir::Program, Diag> {
    let mut seen: HashMap<&str, Span> = HashMap::new();
    let mut funcs = Vec::new();
    let mut main_returns = None;
    for item in &prog.items {
        match item {
            Item::Func(f) => {
                if seen.insert(&f.name, f.name_span).is_some() {
                    return Err(Diag::new(
                        f.name_span,
                        format!("関数 `{}` が 2 回定義されています", f.name),
                    ));
                }
                let lowered = lower_func(f)?;
                if f.name == "main" {
                    main_returns = Some(check_main(f)?);
                }
                funcs.push(lowered);
            }
            Item::Global(d) => return Err(not_yet(d.name_span, "グローバル変数", 4)),
        }
    }
    let Some(main_returns) = main_returns else {
        return Err(Diag::new(Span::new(0, 0), "`main` 関数がありません")
            .hint("プログラムは `function nothing main() { ... }` から始まります"));
    };
    Ok(ir::Program { funcs, main_returns })
}

fn not_yet(span: Span, what: &str, step: u32) -> Diag {
    Diag::new(
        span,
        format!("{}はまだ実装していません（実装の手順 {} で対応）", what, step),
    )
}

/// `main` の形を確かめる。値を返すなら true。
fn check_main(f: &Func) -> Result<bool, Diag> {
    if let Some(p) = f.params.first() {
        return Err(not_yet(p.span, "`main` の引数", 5));
    }
    match &f.ret {
        RetType::Nothing(_) => Ok(false),
        RetType::Type(TypeExpr {
            kind: TypeKind::Int(IntTy::I32),
            ..
        }) => Ok(true),
        RetType::Type(t) => Err(Diag::new(t.span, "`main` の戻り値の型は `32` か `nothing` です")),
    }
}

/// 型を確かめる。今使える型は `32` だけ。
fn int_type(t: &TypeExpr) -> Result<IntTy, Diag> {
    match &t.kind {
        TypeKind::Int(IntTy::I32) => Ok(IntTy::I32),
        TypeKind::Int(IntTy::U8) => Err(not_yet(t.span, "`8` 型", 6)),
        TypeKind::Int(IntTy::I64) => Err(not_yet(t.span, "`64` 型", 6)),
        TypeKind::Bool => Err(not_yet(t.span, "`bool` 型", 4)),
        TypeKind::Decnum => Err(not_yet(t.span, "`decnum` 型", 6)),
        TypeKind::String | TypeKind::Pointer(_) | TypeKind::Array(_) | TypeKind::Address => {
            Err(not_yet(t.span, "この型", 5))
        }
        TypeKind::Maybe(_) | TypeKind::Named(..) => Err(not_yet(t.span, "この型", 6)),
        TypeKind::Function(..) => Err(not_yet(t.span, "関数ポインタ", 7)),
    }
}

struct FnLower {
    ret: Option<IntTy>,
    insts: Vec<Inst>,
    ntemps: u32,
}

fn lower_func(f: &Func) -> Result<ir::Function, Diag> {
    if let Some(p) = f.params.first() {
        if f.name != "main" {
            return Err(not_yet(p.span, "関数の引数", 4));
        }
    }
    let ret = match &f.ret {
        RetType::Nothing(_) => None,
        RetType::Type(t) => Some(int_type(t)?),
    };
    let mut l = FnLower {
        ret,
        insts: Vec::new(),
        ntemps: 0,
    };
    l.block(&f.body)?;
    if ret.is_some() && !block_returns(&f.body) {
        return Err(Diag::new(
            f.body.close,
            format!("関数 `{}` が値を返さずに終わることがあります", f.name),
        )
        .hint("最後に `return 値;` を書いてください"));
    }
    if ret.is_none() {
        l.insts.push(Inst::Ret(None));
    }
    Ok(ir::Function {
        name: f.name.clone(),
        ret,
        insts: l.insts,
        ntemps: l.ntemps,
    })
}

/// ブロックの終わりまで来ずに、必ず `return` するか。
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
        _ => false,
    }
}

impl FnLower {
    fn temp(&mut self) -> u32 {
        self.ntemps += 1;
        self.ntemps - 1
    }

    fn block(&mut self, b: &Block) -> Result<(), Diag> {
        for s in &b.stmts {
            self.stmt(s)?;
        }
        Ok(())
    }

    fn stmt(&mut self, s: &Stmt) -> Result<(), Diag> {
        match &s.kind {
            StmtKind::Return(None) => {
                if self.ret.is_some() {
                    return Err(Diag::new(s.span, "値を返す関数なので、`return` に値が必要です"));
                }
                self.insts.push(Inst::Ret(None));
            }
            StmtKind::Return(Some(e)) => {
                let Some(ty) = self.ret else {
                    return Err(Diag::new(e.span, "`nothing` の関数では値を `return` できません")
                        .hint("値を返すなら、`function 32 名前(...)` のように戻り値の型を書きます"));
                };
                let v = self.expr(e, ty)?;
                self.insts.push(Inst::Ret(Some(v)));
            }
            StmtKind::Decl(d) => return Err(not_yet(d.name_span, "変数", 4)),
            StmtKind::Assign { .. } => return Err(not_yet(s.span, "代入", 4)),
            StmtKind::If { .. } => return Err(not_yet(s.span, "`if`", 4)),
            StmtKind::While { .. } => return Err(not_yet(s.span, "`while`", 4)),
            StmtKind::Break | StmtKind::Continue => return Err(not_yet(s.span, "`break` と `continue`", 4)),
            StmtKind::Arena(_) => return Err(not_yet(s.span, "`arena`", 7)),
            StmtKind::Expr(e) => return Err(not_yet(e.span, "この文", 4)),
        }
        Ok(())
    }

    /// 式を `ty` 型の値として計算する。整数リテラルの型は `ty` から決まる。
    fn expr(&mut self, e: &Expr, ty: IntTy) -> Result<Operand, Diag> {
        match &e.kind {
            ExprKind::Int(n) => int_literal(*n, false, ty, e.span),
            ExprKind::Char(c) => Err(Diag::new(
                e.span,
                format!(
                    "`'{}'` は `8` 型なので、`{}` 型としては使えません",
                    *c as char,
                    ty.name()
                ),
            )
            .hint(format!("`to:{}('…')` で変換してください", ty.name()))),
            ExprKind::Unary(UnOp::Neg, inner) => {
                // `-2147483648` は、2147483648 が 32 に入らなくても書けるようにする。
                if let ExprKind::Int(n) = inner.kind {
                    return int_literal(n, true, ty, e.span);
                }
                let a = self.expr(inner, ty)?;
                let dst = self.temp();
                self.insts.push(Inst::Neg {
                    dst,
                    ty,
                    a,
                    span: e.span,
                });
                Ok(Operand::Temp(dst))
            }
            ExprKind::Unary(UnOp::Not, inner) => {
                let a = self.expr(inner, ty)?;
                let dst = self.temp();
                self.insts.push(Inst::Not { dst, ty, a });
                Ok(Operand::Temp(dst))
            }
            ExprKind::Binary(op, lhs, rhs) => {
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
                    BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge | BinOp::Eq | BinOp::Ne => {
                        return Err(Diag::new(
                            e.span,
                            format!(
                                "比較 `{}` の結果は `bool` なので、`{}` 型としては使えません",
                                op.as_str(),
                                ty.name()
                            ),
                        ))
                    }
                };
                let a = self.expr(lhs, ty)?;
                let b = self.expr(rhs, ty)?;
                let dst = self.temp();
                self.insts.push(Inst::Bin {
                    dst,
                    op: irop,
                    ty,
                    a,
                    b,
                    span: e.span,
                });
                Ok(Operand::Temp(dst))
            }
            ExprKind::Bool(_) => Err(Diag::new(
                e.span,
                format!("`bool` の値は `{}` 型としては使えません", ty.name()),
            )),
            ExprKind::Float(_) => Err(Diag::new(
                e.span,
                format!("小数は `{}` 型としては使えません", ty.name()),
            )
            .hint("整数に変換するなら `to:32(…)` を使います")),
            ExprKind::Str(_) => Err(Diag::new(
                e.span,
                format!("文字列は `{}` 型としては使えません", ty.name()),
            )),
            ExprKind::Null => Err(Diag::new(
                e.span,
                "`null` は `maybe` の付いた型にだけ入れられます",
            )),
            ExprKind::Var(name) => Err(not_yet(e.span, &format!("変数（`{}`）", name), 4)),
            ExprKind::Call(..) => Err(not_yet(e.span, "関数呼び出し", 4)),
            ExprKind::Index(..) | ExprKind::ArrayLit(_) | ExprKind::ArrayNew(_) => {
                Err(not_yet(e.span, "配列", 5))
            }
            ExprKind::Field(..) => Err(not_yet(e.span, "フィールド", 6)),
            ExprKind::Convert(..) => Err(not_yet(e.span, "`to:型(…)`", 6)),
            ExprKind::SizeOf(..) => Err(not_yet(e.span, "`size(…)`", 5)),
        }
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
