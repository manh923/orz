//! 中間表現（三番地コード）。
//!
//! 一時変数（`Temp`）は 1 回だけ代入する。そのため定数の畳み込みは前から順に見るだけでできる。

use crate::ast::IntTy;
use crate::source::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operand {
    Temp(u32),
    Imm(i64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrBinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Shl,
    Shr,
    And,
    Nand,
    Or,
    Nor,
    Xor,
    Xnor,
}

#[derive(Clone, Debug)]
pub enum Inst {
    /// `dst = a op b`。オーバーフローや 0 除算は実行時に止める。
    Bin {
        dst: u32,
        op: IrBinOp,
        ty: IntTy,
        a: Operand,
        b: Operand,
        span: Span,
    },
    /// `dst = -a`
    Neg {
        dst: u32,
        ty: IntTy,
        a: Operand,
        span: Span,
    },
    /// `dst = not a`（ビットごと）
    Not {
        dst: u32,
        ty: IntTy,
        a: Operand,
    },
    /// `dst = src`
    Copy {
        dst: u32,
        src: Operand,
    },
    Ret(Option<Operand>),
}

#[derive(Debug)]
pub struct Function {
    pub name: String,
    /// `None` は `nothing`。
    pub ret: Option<IntTy>,
    pub insts: Vec<Inst>,
    pub ntemps: u32,
}

#[derive(Debug)]
pub struct Program {
    pub funcs: Vec<Function>,
    /// `main` が値を返すか（返すならそれが終了コード）。
    pub main_returns: bool,
}

/// 実行時エラーの理由。コンパイル時と実行時で同じ言い方を使う。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trap {
    Overflow,
    DivZero,
    NegExp,
    ShiftRange,
}

impl Trap {
    pub fn message(self, ty: IntTy) -> String {
        match self {
            Trap::Overflow => format!(
                "計算の結果が {} の範囲（{}〜{}）を超えました",
                ty.name(),
                min_of(ty),
                max_of(ty)
            ),
            Trap::DivZero => "0 で割りました".to_string(),
            Trap::NegExp => "整数の `^` の指数が負です".to_string(),
            Trap::ShiftRange => format!("シフトの量が 0〜{} の範囲の外です", bits_of(ty) - 1),
        }
    }
}

pub fn bits_of(ty: IntTy) -> u32 {
    match ty {
        IntTy::U8 => 8,
        IntTy::I32 => 32,
        IntTy::I64 => 64,
    }
}

pub fn min_of(ty: IntTy) -> i64 {
    match ty {
        IntTy::U8 => 0,
        IntTy::I32 => i32::MIN as i64,
        IntTy::I64 => i64::MIN,
    }
}

pub fn max_of(ty: IntTy) -> i64 {
    match ty {
        IntTy::U8 => u8::MAX as i64,
        IntTy::I32 => i32::MAX as i64,
        IntTy::I64 => i64::MAX,
    }
}
