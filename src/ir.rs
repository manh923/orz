//! 中間表現（三番地コード）。
//!
//! 一時変数（`Temp`）は 1 回だけ代入し、使う場所より前で必ず代入されている。
//! そのため定数の畳み込みは、前から順に見るだけでできる。
//! 何度も代入する値（ソースの変数、`and` の結果など）は `Var` に置く。
//!
//! 値はすべて 32bit。`bool` は 0 か 1。

use crate::ast::IntTy;
use crate::source::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operand {
    Temp(u32),
    /// 関数の中の変数（引数を含む）。
    Var(u32),
    /// グローバル変数（`Program::globals` の番号）。
    Global(u32),
    Imm(i64),
}

/// 書き込み先。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Var(u32),
    Global(u32),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
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
    /// `dst = not a`（`bool`。0 と 1 を入れ替える）
    BoolNot {
        dst: u32,
        a: Operand,
    },
    /// `dst = (a op b)`。結果は 0 か 1。
    Cmp {
        dst: u32,
        op: CmpOp,
        a: Operand,
        b: Operand,
    },
    /// `dst = src`
    Copy {
        dst: u32,
        src: Operand,
    },
    /// 変数に書く。
    Store {
        place: Place,
        src: Operand,
    },
    /// 関数を呼ぶ。`func` は `Program::funcs` の番号。
    Call {
        dst: Option<u32>,
        func: u32,
        args: Vec<Operand>,
    },
    Label(u32),
    Jump(u32),
    /// `cond` が 0 でなければ `then` へ、0 なら `els` へ。
    Branch {
        cond: Operand,
        then: u32,
        els: u32,
    },
    Ret(Option<Operand>),
}

#[derive(Debug)]
pub struct Function {
    pub name: String,
    /// 値を返すか（`nothing` でないか）。
    pub returns_value: bool,
    /// 引数の数。引数は `Var(0)` から順に入る。
    pub nparams: u32,
    pub insts: Vec<Inst>,
    pub ntemps: u32,
    pub nvars: u32,
}

#[derive(Debug)]
pub struct Global {
    pub name: String,
    pub init: i64,
}

#[derive(Debug)]
pub struct Program {
    pub funcs: Vec<Function>,
    pub globals: Vec<Global>,
    /// `main` の番号。
    pub main: u32,
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
            Trap::Overflow => {
                format!(
                    "計算の結果が {} の範囲（{}〜{}）を超えました",
                    ty.name(),
                    min_of(ty),
                    max_of(ty)
                )
            }
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
