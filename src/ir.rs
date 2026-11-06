//! 中間表現（三番地コード）。
//!
//! 一時変数（`Temp`）は 1 回だけ代入し、使う場所より前で必ず代入されている。
//! そのため定数の畳み込みは、前から順に見るだけでできる。
//! 何度も代入する値（ソースの変数、`and` の結果など）は `Var` に置く。
//!
//! 値はすべて 8 バイトの場所に置く。`32` は符号を広げ、`8` と `bool` は 0 を詰めた形にそろえておく。
//! アドレス（string・array・pointer・address・構造体）と `decnum`（ビットのまま）は 8 バイト。

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
    /// 文字列リテラルのアドレス（`Program::strings` の番号）。
    Str(u32),
}

/// 変数の書き込み先。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Var(u32),
    Global(u32),
}

/// メモリ上の幅。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Width {
    /// 1 バイト（読むときは 0 を詰める）。`8` と `bool`。
    W1,
    /// 4 バイト（読むときは符号を広げる）。`32`。
    W4,
    /// 8 バイト。`64`、`decnum` とアドレス。
    W8,
}

impl Width {
    pub fn bytes(self) -> i64 {
        match self {
            Width::W1 => 1,
            Width::W4 => 4,
            Width::W8 => 8,
        }
    }
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

/// `decnum` の二項演算。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
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

/// ランタイム（runtime/orz_runtime.c）の関数。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rt {
    /// (n) → n バイトのメモリ。中身は 0
    Alloc,
    /// (要素数, 要素の大きさ, 初期値の種類) → 配列
    ArrayNew,
    /// (底, 指数) → 累乗
    Pow(IntTy),
    /// (a, b) → a と b をつないだ文字列
    Concat,
    /// (a, b) → 中身が同じなら 1
    StrEq,
    StrFromInt,
    StrFromBool,
    /// (decnum のビット) → 文字列
    StrFromDec,
    /// (文字列, ビット数) → maybe の整数（箱に入れたもの）
    StrToInt,
    /// (文字列) → maybe:decnum（箱に入れたもの）
    StrToDec,
    /// (値, 型の説明) → 表示する
    Put,
    Newline,
    ReadLine,
    ReadFile,
    WriteFile,
    LastError,
    /// (値) → 値を入れた 8 バイトの箱（maybe:32 などに使う）
    Box,
    /// (構造体, 大きさ) → 同じ中身の新しい構造体
    Copy,
    /// (配列, 要素の大きさ) → 同じ中身の新しい配列
    ArrayCopy,
}

impl Rt {
    pub fn symbol(self) -> &'static str {
        match self {
            Rt::Alloc => "orz_rt_alloc",
            Rt::ArrayNew => "orz_rt_array_new",
            Rt::Pow(_) => "orz_rt_pow",
            Rt::Concat => "orz_rt_str_concat",
            Rt::StrEq => "orz_rt_str_eq",
            Rt::StrFromInt => "orz_rt_str_from_int",
            Rt::StrFromBool => "orz_rt_str_from_bool",
            Rt::StrFromDec => "orz_rt_str_from_dec",
            Rt::StrToInt => "orz_rt_str_to_int",
            Rt::StrToDec => "orz_rt_str_to_dec",
            Rt::Put => "orz_rt_put",
            Rt::Newline => "orz_rt_newline",
            Rt::ReadLine => "orz_rt_read_line",
            Rt::ReadFile => "orz_rt_read_file",
            Rt::WriteFile => "orz_rt_write_file",
            Rt::LastError => "orz_rt_last_error",
            Rt::Box => "orz_rt_box",
            Rt::Copy => "orz_rt_copy",
            Rt::ArrayCopy => "orz_rt_array_copy",
        }
    }

    /// 実行時エラーのときに「ファイル名:行」を表示するために、場所を最後の引数で渡すか。
    pub fn takes_site(self) -> bool {
        matches!(self, Rt::Alloc | Rt::ArrayNew | Rt::Pow(_))
    }
}

#[rustfmt::skip]
#[derive(Clone, Debug)]
pub enum Inst {
    /// `dst = a op b`。オーバーフローや 0 除算は実行時に止める。
    Bin { dst: u32, op: IrBinOp, ty: IntTy, a: Operand, b: Operand, span: Span },
    /// `dst = -a`
    Neg { dst: u32, ty: IntTy, a: Operand, span: Span },
    /// `dst = not a`（ビットごと）
    Not { dst: u32, ty: IntTy, a: Operand },
    /// `dst = not a`（`bool`。0 と 1 を入れ替える）
    BoolNot { dst: u32, a: Operand },
    /// `dst = (a op b)`。8 バイトの符号ありで比べる。結果は 0 か 1。
    Cmp { dst: u32, op: CmpOp, a: Operand, b: Operand },
    /// 整数の型を変える。範囲に入らなければ実行時に止める。
    Convert { dst: u32, from: IntTy, to: IntTy, a: Operand, span: Span },
    /// `decnum` の二項演算。結果が無限大などになれば実行時に止める。
    FBin { dst: u32, op: FOp, a: Operand, b: Operand, span: Span },
    /// `dst = -a`（decnum）
    FNeg { dst: u32, a: Operand },
    /// `dst = (a op b)`（decnum）。結果は 0 か 1。
    FCmp { dst: u32, op: CmpOp, a: Operand, b: Operand },
    /// 整数 → decnum
    IntToDec { dst: u32, a: Operand },
    /// decnum → 整数（0 の方向に切り捨て）。範囲に入らなければ実行時に止める。
    DecToInt { dst: u32, to: IntTy, a: Operand, span: Span },
    /// `dst = src`
    Copy { dst: u32, src: Operand },
    /// 変数に書く。
    Store { place: Place, src: Operand },
    /// `dst = *addr`
    Load { dst: u32, addr: Operand, width: Width },
    /// `*addr = src`
    StoreMem { addr: Operand, src: Operand, width: Width },
    /// `dst = base + index * scale`
    ElemAddr { dst: u32, base: Operand, index: Operand, scale: i64 },
    /// `0 <= index < 配列の長さ` でなければ実行時に止める。
    CheckIndex { array: Operand, index: Operand, span: Span },
    /// `dst = 長さ`（string と array。先頭の手前の 8 バイト）
    Length { dst: u32, a: Operand },
    /// 関数を呼ぶ。`func` は `Program::funcs` の番号。
    Call { dst: Option<u32>, func: u32, args: Vec<Operand> },
    /// ランタイムの関数を呼ぶ。
    CallRt { dst: Option<u32>, func: Rt, args: Vec<Operand>, span: Span },
    Label(u32),
    Jump(u32),
    /// `cond` が 0 でなければ `then` へ、0 なら `els` へ。
    Branch { cond: Operand, then: u32, els: u32 },
    Ret(Option<Operand>),
}

#[derive(Debug)]
pub struct Function {
    pub name: String,
    /// アセンブリでの名前（ファイルごとに区別する）。
    pub symbol: String,
    /// 値を返すか（`nothing` でないか）。
    pub returns_value: bool,
    /// 引数の数。引数は `Var(0)` から順に入る。
    pub nparams: u32,
    pub insts: Vec<Inst>,
    pub ntemps: u32,
    pub nvars: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConstVal {
    Int(i64),
    /// 文字列リテラル（`Program::strings` の番号）。
    Str(u32),
    /// 値を入れた箱（`maybe:32` などの、null でない初期値）。
    Boxed(i64),
}

#[derive(Debug)]
pub struct Global {
    /// アセンブリでの名前（ファイルごとに区別する）。
    pub symbol: String,
    pub init: ConstVal,
}

#[derive(Debug)]
pub struct Program {
    pub funcs: Vec<Function>,
    pub globals: Vec<Global>,
    /// 文字列リテラル。
    pub strings: Vec<Vec<u8>>,
    /// 構造体を表示するための説明（ランタイムの orz_rt_put が読む）。
    pub struct_descs: Vec<Vec<u8>>,
    /// `main` の番号。
    pub main: u32,
    /// `main` が値を返すか（返すならそれが終了コード）。
    pub main_returns: bool,
    /// `main` がコマンドライン引数を受け取るか。
    pub main_takes_args: bool,
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
            Trap::Overflow => format!("計算の結果が {}を超えました", range_text(ty)),
            Trap::DivZero => "0 で割りました".to_string(),
            Trap::NegExp => "整数の `^` の指数が負です".to_string(),
            Trap::ShiftRange => format!("シフトの量が 0〜{} の範囲の外です", bits_of(ty) - 1),
        }
    }
}

/// decnum の計算の結果が、無限大などになったとき。
pub const DEC_OVERFLOW: &str = "計算の結果が decnum の範囲を超えました";

/// 「32 の範囲（-2147483648〜2147483647）」
pub fn range_text(ty: IntTy) -> String {
    format!("{} の範囲（{}〜{}）", ty.name(), min_of(ty), max_of(ty))
}

/// 変換で範囲外のときの文の後半。
pub fn convert_message(to: IntTy) -> String {
    format!("{}に入りません", range_text(to))
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

/// decnum を整数に変換するとき、切り捨てた結果が範囲に入るか。
pub fn dec_fits(v: f64, to: IntTy) -> bool {
    match to {
        IntTy::U8 => v > -1.0 && v < 256.0,
        IntTy::I32 => v > -2147483649.0 && v < 2147483648.0,
        IntTy::I64 => (-9223372036854775808.0..9223372036854775808.0).contains(&v),
    }
}
