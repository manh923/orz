//! 構文木。
//!
//! 手順 4 以降で使う形も先に入れてあるので、まだ読まれないフィールドがある。
#![allow(dead_code)]

use crate::source::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IntTy {
    /// `8`：符号なし 8bit
    U8,
    /// `32`：符号あり 32bit
    I32,
    /// `64`：符号あり 64bit
    I64,
}

impl IntTy {
    pub fn name(self) -> &'static str {
        match self {
            IntTy::U8 => "8",
            IntTy::I32 => "32",
            IntTy::I64 => "64",
        }
    }
}

/// ソースに書かれた型。`Map:string:List:32` のような `:` で区切った並びのまま持つ。
/// どこで区切るか（`List` が型をいくつ取るか）は、定義を見ないとわからないので、意味解析で決める。
#[derive(Clone, Debug)]
pub struct TypeExpr {
    pub atoms: Vec<TypeAtom>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct TypeAtom {
    pub kind: AtomKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum AtomKind {
    Int(IntTy),
    Bool,
    Decnum,
    String,
    Address,
    /// 後ろに型を 1 つ取る。
    Pointer,
    Array,
    Maybe,
    /// `function:戻り値の型(引数の型, ...)`
    Function(Box<RetType>, Vec<TypeExpr>),
    /// 構造体か、ジェネリクスの型引数（`T`）。`util.Point` のように名前空間が付くこともある。
    Named(Option<String>, String),
}

#[derive(Clone, Debug)]
pub enum RetType {
    Nothing(Span),
    Type(TypeExpr),
}

impl RetType {
    pub fn span(&self) -> Span {
        match self {
            RetType::Nothing(s) => *s,
            RetType::Type(t) => t.span,
        }
    }
}

#[derive(Debug)]
pub struct Program {
    pub items: Vec<Item>,
}

#[derive(Debug)]
pub enum Item {
    Func(Func),
    Global(Decl),
    Import(Import),
    Struct(StructDef),
}

#[derive(Debug)]
pub struct Import {
    pub target: ImportTarget,
    /// 自分のファイルの import なら、読み込んだファイルの番号（読み込むときに埋める）。
    pub resolved: Option<usize>,
    /// `as` で付けた名前。
    pub alias: Option<String>,
    pub span: Span,
}

#[derive(Debug)]
pub enum ImportTarget {
    /// `import stdio;`（標準ライブラリ）
    Std(String),
    /// `import "util.orz";`（自分のファイル）
    File(String),
}

#[derive(Debug)]
pub struct Func {
    pub name: String,
    pub name_span: Span,
    pub ret: RetType,
    pub params: Vec<Param>,
    pub body: Block,
    /// `export` が付いているか。
    pub exported: bool,
    /// ジェネリクスの型引数（`function T get:T(…)` の `T`）。
    pub type_params: Vec<String>,
    /// `extern` の関数（中身はランタイムや C の側にある）。
    pub is_extern: bool,
    /// `extern "kernel32.dll" function …` の DLL の名前（Windows の DLL の関数）。
    pub dll: Option<String>,
}

/// `struct Point { x:32; y:32; }`
#[derive(Debug)]
pub struct StructDef {
    pub name: String,
    pub name_span: Span,
    pub fields: Vec<FieldDef>,
    pub exported: bool,
    /// ジェネリクスの型引数（`struct List:T` の `T`）。
    pub type_params: Vec<String>,
}

#[derive(Debug)]
pub struct FieldDef {
    pub name: String,
    pub ty: TypeExpr,
    pub span: Span,
}

#[derive(Debug)]
pub struct Param {
    pub name: String,
    pub ty: TypeExpr,
    pub span: Span,
}

#[derive(Debug)]
pub struct Decl {
    pub name: String,
    pub name_span: Span,
    pub ty: TypeExpr,
    pub init: Expr,
    /// `immutable` が付いているか（グローバル変数だけ）。
    pub immutable: bool,
    /// `export` が付いているか（グローバル変数だけ）。
    pub exported: bool,
}

#[derive(Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    /// 閉じ `}` の位置。
    pub close: Span,
}

#[derive(Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Debug)]
pub enum StmtKind {
    Decl(Decl),
    Assign {
        target: Expr,
        value: Expr,
    },
    If {
        cond: Expr,
        then: Block,
        els: Option<Else>,
    },
    While {
        cond: Expr,
        body: Block,
    },
    Break,
    Continue,
    Return(Option<Expr>),
    Arena(Block),
    Expr(Expr),
}

#[derive(Debug)]
pub enum Else {
    Block(Block),
    /// `else if`
    If(Box<Stmt>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Shl,
    Shr,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
    And,
    Nand,
    Or,
    Nor,
    Xor,
    Xnor,
}

impl BinOp {
    pub fn as_str(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Mod => "%",
            BinOp::Pow => "^",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
            BinOp::Lt => "<",
            BinOp::Gt => ">",
            BinOp::Le => "<=",
            BinOp::Ge => ">=",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::And => "and",
            BinOp::Nand => "nand",
            BinOp::Or => "or",
            BinOp::Nor => "nor",
            BinOp::Xor => "xor",
            BinOp::Xnor => "xnor",
        }
    }
}

#[derive(Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug)]
pub enum ExprKind {
    Int(u64),
    Float(f64),
    Str(Vec<u8>),
    Char(u8),
    Bool(bool),
    Null,
    Var(String),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>),
    Index(Box<Expr>, Box<Expr>),
    Field(Box<Expr>, String),
    /// `to:型(x)`
    Convert(TypeExpr, Box<Expr>),
    /// `size(:型)`
    SizeOf(TypeExpr),
    /// `[1, 2, 3]`
    ArrayLit(Vec<Expr>),
    /// `array(n)`
    /// `array(n)`、`array(n, 初期値)`
    ArrayNew(Box<Expr>, Option<Box<Expr>>),
    /// `get:32`、`List:32` のように型を付けた関数・構造体の名前。
    Generic(Box<Expr>, TypeExpr),
}
