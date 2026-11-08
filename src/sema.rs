//! 型の確認と、中間表現への変換。
//!
//! 今できるのは手順 7 の途中まで：
//!   整数（`8`・`32`・`64`）、`bool`、`decnum`、`address`、`string`、`array`、`pointer`、構造体、
//!   `maybe` と `null`、`to:型(x)`、変数、`if`、`while`、関数呼び出し、グローバル変数、
//!   `import stdio;` と自分のファイルの `import`・`export`、関数ポインタ、ジェネリクス。
//! それ以外の書き方は、構文としては読めても「まだ実装していません」のエラーにする。
//!
//! ファイル（モジュール）は 0 番が最初に渡されたファイルで、`Span::file` もこの番号になる。
//!
//! ジェネリクスは、使われた型ごとに別の関数・構造体としてコンパイルする。
//! ジェネリクスの関数は、定義の時点で一度、`T` を中身のわからない型（`Ty::Param`）として確かめる。
//! そのため `T` に `+` や `<` を使うと、使われる前でもエラーになる。

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::ast::*;
use crate::fold;
use crate::ir::{self, CmpOp, ConstVal, FOp, Inst, IrBinOp, Operand, Place, Rt, Width};
use crate::source::{Diag, Span};

/// 値の型。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Ty {
    Int(IntTy),
    Address,
    Bool,
    Dec,
    Str,
    Pointer(Box<Ty>),
    Array(Box<Ty>),
    Maybe(Box<Ty>),
    /// 構造体（番号と名前）。値はアドレス。
    Struct(usize, Rc<str>),
    /// 関数ポインタ（戻り値の型、引数の型）。戻り値の `None` は `nothing`。
    Func(Option<Box<Ty>>, Vec<Ty>),
    /// ジェネリクスの型引数。定義を確かめるときだけ使う、中身のわからない型。
    Param(u32, Rc<str>),
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
            Ty::Dec => "decnum".into(),
            Ty::Str => "string".into(),
            Ty::Pointer(t) => format!("pointer:{}", t.name()),
            Ty::Array(t) => format!("array:{}", t.name()),
            Ty::Maybe(t) => format!("maybe:{}", t.name()),
            Ty::Struct(_, name) | Ty::Param(_, name) => name.to_string(),
            Ty::Func(ret, params) => {
                let ret = ret.as_ref().map(|t| t.name()).unwrap_or_else(|| "nothing".into());
                let params: Vec<String> = params.iter().map(|t| t.name()).collect();
                format!("function:{}({})", ret, params.join(", "))
            }
            Ty::Null => "null".into(),
        }
    }

    /// 整数として計算するときの型。`address` は 64 として計算する。
    fn int_like(&self) -> Option<IntTy> {
        match self {
            Ty::Int(t) => Some(*t),
            Ty::Address => Some(IntTy::I64),
            _ => None,
        }
    }

    fn is_numeric(&self) -> bool {
        self.int_like().is_some() || *self == Ty::Dec
    }

    /// メモリ上の幅（配列の要素、ポインタの先、構造体のフィールド）。
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

    /// `maybe` を付けたとき、値を箱に入れるか（整数などの値の型）。
    /// string・array・pointer・構造体・関数はアドレスなので、null を 0 で表せば箱は要らない。
    fn needs_box(&self) -> bool {
        matches!(self, Ty::Int(_) | Ty::Bool | Ty::Address | Ty::Dec)
    }

    /// どこかで作った値（arena の中で作られうるもの）のアドレスか。arena の持ち出しの検査に使う。
    /// maybe:32 などの箱は、arena の中でも GC 側に作るので対象外。関数はプログラムの中にあるので対象外。
    fn has_level(&self) -> bool {
        match self {
            Ty::Str | Ty::Array(_) | Ty::Pointer(_) | Ty::Struct(..) | Ty::Address | Ty::Param(..) => true,
            Ty::Maybe(inner) => !inner.needs_box() && inner.has_level(),
            _ => false,
        }
    }

    /// 値の中にポインタがあるか（GC が中を調べる必要があるか）。数と bool だけはない。
    fn has_pointers(&self) -> bool {
        !matches!(self, Ty::Int(_) | Ty::Bool | Ty::Dec)
    }

    fn contains_param(&self) -> bool {
        match self {
            Ty::Param(..) => true,
            Ty::Pointer(t) | Ty::Array(t) | Ty::Maybe(t) => t.contains_param(),
            Ty::Func(r, ps) => {
                r.as_ref().is_some_and(|t| t.contains_param()) || ps.iter().any(Ty::contains_param)
            }
            _ => false,
        }
    }

    /// 表示するときの型の説明（ランタイムの orz_rt_put が読む）。
    fn desc(&self) -> String {
        match self {
            Ty::Int(IntTy::U8) => "b".into(),
            Ty::Int(IntTy::I32) => "i".into(),
            Ty::Int(IntTy::I64) => "l".into(),
            Ty::Bool => "B".into(),
            Ty::Dec => "d".into(),
            Ty::Str => "s".into(),
            Ty::Pointer(_) | Ty::Address | Ty::Func(..) | Ty::Param(..) => "p".into(),
            Ty::Null => "n".into(),
            Ty::Maybe(t) if t.needs_box() => format!("M{}", t.desc()),
            Ty::Maybe(t) => format!("m{}", t.desc()),
            Ty::Array(t) => format!("[{}", t.desc()),
            Ty::Struct(i, _) => format!("S{};", i),
        }
    }
}

/// `from` の値を `to` 型の場所に入れられるか。
fn assignable(to: &Ty, from: &Ty) -> bool {
    to == from || matches!(to, Ty::Maybe(inner) if **inner == *from || *from == Ty::Null)
}

/// `import` なしで使える組み込みの関数。
const BUILTINS: &[&str] = &["allocate", "length", "size", "bytes", "copy", "collect"];

/// 1 つの関数の中の arena の入れ子の上限。
pub const MAX_ARENA_DEPTH: u32 = 16;

/// ジェネリクスを展開しすぎないための上限（型が無限に大きくなる書き方を止める）。
const MAX_INSTANCES: usize = 200;

/// ジェネリクスの型引数と、その型。
type TParams = Vec<(String, Ty)>;

// ---------------- プログラム全体 ----------------

#[derive(Clone)]
struct FuncSig {
    name: String,
    params: Vec<Ty>,
    /// `None` は `nothing`。
    ret: Option<Ty>,
    exported: bool,
    symbol: String,
}

impl FuncSig {
    fn ty(&self) -> Ty {
        Ty::Func(self.ret.clone().map(Box::new), self.params.clone())
    }
}

/// コンパイルする関数（普通の関数と、ジェネリクスを展開したもの）。
struct FuncInfo<'p> {
    sig: FuncSig,
    ast: &'p Func,
    module: usize,
    tparams: TParams,
}

/// ジェネリクスの関数の定義。
struct GenericFunc<'p> {
    ast: &'p Func,
    module: usize,
    /// 引数と戻り値の型（型引数は `Ty::Param` のまま）。
    params: Vec<Ty>,
    ret: Option<Ty>,
    instances: usize,
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
    module: usize,
    ty: Ty,
    state: GlobalState,
    symbol: String,
}

struct FieldInfo {
    name: String,
    ty: Ty,
    offset: i64,
}

/// 入れ場所の種類：構造体のフィールド（構造体, 何番目）か、array・pointer の要素（要素の型）。
#[derive(PartialEq, Eq, Hash)]
enum SlotKey {
    Field(usize, usize),
    Elem(Ty),
}

struct StructInfo<'p> {
    def: &'p StructDef,
    module: usize,
    /// 表示用の名前（`List:32` など）。
    name: Rc<str>,
    fields: Vec<FieldInfo>,
    size: i64,
    /// ジェネリクスを展開したものなら、どの定義をどの型で展開したか。
    origin: Option<(usize, Vec<Ty>)>,
}

/// ジェネリクスの構造体の定義。
struct GenericStruct<'p> {
    def: &'p StructDef,
    module: usize,
    instances: usize,
}

#[derive(Clone, Copy)]
enum TopName {
    Func(usize),
    GenericFunc(usize),
    Global(usize),
    Struct(usize),
    GenericStruct(usize),
}

/// `import` した名前空間が指すもの。
#[derive(Clone, Copy, PartialEq, Eq)]
enum ModRef {
    Stdio,
    File(usize),
}

struct ModInfo<'p> {
    /// このファイルで定義した名前。
    names: HashMap<&'p str, TopName>,
    /// 名前空間の名前 → import したもの
    imports: HashMap<String, ModRef>,
}

/// 名前（`name` か `ns.name`）が指すもの。
enum Target {
    Stdio,
    Top(TopName),
}

struct Ctx<'p> {
    mods: Vec<ModInfo<'p>>,
    funcs: Vec<FuncInfo<'p>>,
    generic_funcs: Vec<GenericFunc<'p>>,
    func_instances: HashMap<(usize, Vec<Ty>), usize>,
    /// 中身をまだ変換していない、展開したジェネリクスの関数。
    pending: Vec<usize>,
    globals: Vec<GlobalInfo<'p>>,
    structs: Vec<StructInfo<'p>>,
    generic_structs: Vec<GenericStruct<'p>>,
    struct_instances: HashMap<(usize, Vec<Ty>), usize>,
    strings: Vec<Vec<u8>>,
    string_ids: HashMap<Vec<u8>, u32>,
    /// グローバル変数の初期化の関数（main の前に、この順に呼ぶ）。
    global_inits: Vec<ir::Function>,
    /// 入れ場所の種類の番号（`ir::Function::slots`）。
    slot_ids: HashMap<SlotKey, u32>,
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

    fn top(&self, m: usize, name: &str) -> Option<TopName> {
        self.mods[m].names.get(name).copied()
    }

    /// 名前が組み込みの関数・名前空間とぶつかるか。
    fn reserved(&self, m: usize, span: Span, name: &str, what: &str) -> Result<(), Diag> {
        if BUILTINS.contains(&name) {
            return Err(Diag::new(
                span,
                format!("`{}` は組み込みの関数の名前なので、{}には使えません", name, what),
            ));
        }
        if self.mods[m].imports.contains_key(name) {
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

    fn struct_ty(&self, i: usize) -> Ty {
        Ty::Struct(i, self.structs[i].name.clone())
    }

    /// t 型の値が、中に別のもの（構造体・配列など、arena の中で作られうるもの）を入れられるか。
    fn is_deep(&self, t: &Ty) -> bool {
        match t {
            Ty::Struct(i, _) => self.structs[*i].fields.iter().any(|f| f.ty.has_level()),
            Ty::Array(e) | Ty::Pointer(e) => e.has_level(),
            Ty::Maybe(inner) => self.is_deep(inner),
            _ => false,
        }
    }

    /// 構造体 s の j 番目のフィールドの、入れ場所の種類。中に別のものを入れられない型なら空。
    fn field_slots(&mut self, s: usize, j: usize) -> Vec<u32> {
        let t = self.structs[s].fields[j].ty.clone();
        self.slots_for(SlotKey::Field(s, j), &t)
    }

    /// t 型の要素（array と pointer）の、入れ場所の種類。
    fn elem_slots(&mut self, t: &Ty) -> Vec<u32> {
        let key = match t {
            Ty::Maybe(inner) => (**inner).clone(),
            _ => t.clone(),
        };
        self.slots_for(SlotKey::Elem(key), t)
    }

    fn slots_for(&mut self, key: SlotKey, t: &Ty) -> Vec<u32> {
        if !self.is_deep(t) {
            return Vec::new();
        }
        let n = self.slot_ids.len() as u32;
        vec![*self.slot_ids.entry(key).or_insert(n)]
    }

    /// `copy` で複製する中身の、入れ場所の種類すべて。
    fn copy_slots(&mut self, t: &Ty) -> Vec<u32> {
        match t {
            Ty::Struct(i, _) => (0..self.structs[*i].fields.len())
                .flat_map(|j| self.field_slots(*i, j))
                .collect(),
            Ty::Array(e) => self.elem_slots(e),
            _ => Vec::new(),
        }
    }

    fn exported(&self, top: TopName) -> bool {
        match top {
            TopName::Func(i) => self.funcs[i].sig.exported,
            TopName::GenericFunc(g) => self.generic_funcs[g].ast.exported,
            TopName::Global(i) => self.globals[i].decl.exported,
            TopName::Struct(i) => self.structs[i].def.exported,
            TopName::GenericStruct(g) => self.generic_structs[g].def.exported,
        }
    }

    /// `ns.name` を探す。ほかのファイルのものは `export` されていなければ使えない。
    fn member(&self, m: usize, ns: &str, name: &str, span: Span) -> Result<Target, Diag> {
        let Some(r) = self.mods[m].imports.get(ns).copied() else {
            return Err(
                Diag::new(span, format!("`{}` という名前空間はありません", ns)).hint(format!(
                    "`import {};` か `import \"{}.orz\";` を書き忘れていませんか",
                    ns, ns
                )),
            );
        };
        let m2 = match r {
            ModRef::Stdio => return Ok(Target::Stdio),
            ModRef::File(m2) => m2,
        };
        let Some(top) = self.top(m2, name) else {
            return Err(Diag::new(span, format!("`{}` に `{}` はありません", ns, name)));
        };
        if !self.exported(top) {
            return Err(
                Diag::new(span, format!("`{}` の `{}` は export されていません", ns, name))
                    .hint("ほかのファイルから使わせたいなら、定義の前に `export` を付けます"),
            );
        }
        Ok(Target::Top(top))
    }

    /// 名前（`name` または `ns.name`）を探す。
    fn find(&self, m: usize, ns: Option<&str>, name: &str, span: Span) -> Result<Option<Target>, Diag> {
        match ns {
            Some(ns) => self.member(m, ns, name, span).map(Some),
            None => Ok(self.top(m, name).map(Target::Top)),
        }
    }

    /// ソースに書かれた型を確かめる。`m` は書かれているファイル、`tp` はジェネリクスの型引数。
    fn resolve_type(&mut self, m: usize, t: &TypeExpr, tp: &TParams) -> Result<Ty, Diag> {
        let mut pos = 0;
        let ty = self.resolve_atoms(m, &t.atoms, &mut pos, tp)?;
        if pos < t.atoms.len() {
            return Err(Diag::new(t.atoms[pos].span, "型の後ろに、余分な `:…` があります")
                .hint("型引数の数が、定義と合っているか確かめてください"));
        }
        Ok(ty)
    }

    /// `:` で並んだ型を、型引数の数に従って 1 つ読む。
    fn resolve_atoms(
        &mut self,
        m: usize,
        atoms: &[TypeAtom],
        pos: &mut usize,
        tp: &TParams,
    ) -> Result<Ty, Diag> {
        let a = &atoms[*pos];
        *pos += 1;
        let next = |ctx: &mut Self, pos: &mut usize, what: &str| -> Result<Ty, Diag> {
            if *pos >= atoms.len() {
                return Err(Diag::new(a.span, format!("{}の後に `:` と型が必要です", what)));
            }
            ctx.resolve_atoms(m, atoms, pos, tp)
        };
        Ok(match &a.kind {
            AtomKind::Int(it) => Ty::Int(*it),
            AtomKind::Bool => Ty::Bool,
            AtomKind::Decnum => Ty::Dec,
            AtomKind::String => Ty::Str,
            AtomKind::Address => Ty::Address,
            AtomKind::Pointer => Ty::Pointer(Box::new(next(self, pos, "`pointer` ")?)),
            AtomKind::Array => Ty::Array(Box::new(next(self, pos, "`array` ")?)),
            AtomKind::Maybe => match next(self, pos, "`maybe` ")? {
                Ty::Maybe(_) => return Err(Diag::new(a.span, "`maybe` は重ねて付けられません")),
                i => Ty::Maybe(Box::new(i)),
            },
            AtomKind::Function(ret, params) => {
                let ret = match &**ret {
                    RetType::Nothing(_) => None,
                    RetType::Type(t) => Some(Box::new(self.resolve_type(m, t, tp)?)),
                };
                let mut ps = Vec::new();
                for p in params {
                    ps.push(self.resolve_type(m, p, tp)?);
                }
                Ty::Func(ret, ps)
            }
            AtomKind::Named(None, name) if tp.iter().any(|(n, _)| n == name) => {
                tp.iter().find(|(n, _)| n == name).unwrap().1.clone()
            }
            AtomKind::Named(ns, name) => {
                let shown = match ns {
                    Some(ns) => format!("{}.{}", ns, name),
                    None => name.clone(),
                };
                match self.find(m, ns.as_deref(), name, a.span)? {
                    Some(Target::Top(TopName::Struct(i))) => self.struct_ty(i),
                    Some(Target::Top(TopName::GenericStruct(g))) => {
                        let n = self.generic_structs[g].def.type_params.len();
                        let mut args = Vec::new();
                        for _ in 0..n {
                            if *pos >= atoms.len() {
                                return Err(Diag::new(
                                    a.span,
                                    format!("`{}` には型を {} つ付けます（例：{}:32）", shown, n, shown),
                                ));
                            }
                            args.push(self.resolve_atoms(m, atoms, pos, tp)?);
                        }
                        let i = self.instantiate_struct(g, args, a.span)?;
                        self.struct_ty(i)
                    }
                    Some(_) => return Err(Diag::new(a.span, format!("`{}` は型ではありません", shown))),
                    None => return Err(unknown_type(a.span, name)),
                }
            }
        })
    }

    /// 構造体のフィールドの型を決めて、C と同じ並べ方（それぞれの幅にそろえる）で並べる。
    fn resolve_fields(&mut self, i: usize, tp: &TParams) -> Result<(), Diag> {
        let (def, m) = (self.structs[i].def, self.structs[i].module);
        let mut fields: Vec<FieldInfo> = Vec::new();
        let mut offset = 0i64;
        let mut align = 1i64;
        for f in &def.fields {
            if fields.iter().any(|x| x.name == f.name) {
                return Err(Diag::new(
                    f.span,
                    format!("フィールド `{}` が 2 つあります", f.name),
                ));
            }
            let ty = self.resolve_type(m, &f.ty, tp)?;
            let w = ty.width().bytes();
            offset = (offset + w - 1) / w * w;
            align = align.max(w);
            fields.push(FieldInfo {
                name: f.name.clone(),
                ty,
                offset,
            });
            offset += w;
        }
        self.structs[i].size = (offset + align - 1) / align * align;
        self.structs[i].fields = fields;
        check_struct_cycle(self, i)
    }

    /// ジェネリクスの構造体を、型を決めて展開する（同じ型なら前に作ったものを使う）。
    fn instantiate_struct(&mut self, g: usize, args: Vec<Ty>, span: Span) -> Result<usize, Diag> {
        let key = (g, args.clone());
        if let Some(i) = self.struct_instances.get(&key) {
            return Ok(*i);
        }
        let (def, module) = (self.generic_structs[g].def, self.generic_structs[g].module);
        self.generic_structs[g].instances += 1;
        if self.generic_structs[g].instances > MAX_INSTANCES {
            return Err(
                Diag::new(span, format!("構造体 `{}` の展開が多すぎます", def.name))
                    .hint("型引数が、展開するたびに大きくなっていませんか"),
            );
        }
        let shown: Vec<String> = args.iter().map(Ty::name).collect();
        let i = self.structs.len();
        self.structs.push(StructInfo {
            def,
            module,
            name: Rc::from(format!("{}:{}", def.name, shown.join(":"))),
            fields: Vec::new(),
            size: 0,
            origin: Some((g, args.clone())),
        });
        // 先に登録しておく（自分自身を maybe で含む構造体のため）。
        self.struct_instances.insert(key, i);
        let tp: TParams = def.type_params.iter().cloned().zip(args).collect();
        self.resolve_fields(i, &tp)?;
        Ok(i)
    }

    /// ジェネリクスの関数を、型を決めて展開する（同じ型なら前に作ったものを使う）。
    fn instantiate_func(&mut self, g: usize, args: Vec<Ty>, span: Span) -> Result<usize, Diag> {
        let key = (g, args.clone());
        if let Some(i) = self.func_instances.get(&key) {
            return Ok(*i);
        }
        let (ast, module) = (self.generic_funcs[g].ast, self.generic_funcs[g].module);
        self.generic_funcs[g].instances += 1;
        if self.generic_funcs[g].instances > MAX_INSTANCES {
            return Err(Diag::new(span, format!("関数 `{}` の展開が多すぎます", ast.name))
                .hint("型引数が、呼ぶたびに大きくなっていませんか"));
        }
        let tp: TParams = ast.type_params.iter().cloned().zip(args.clone()).collect();
        let (params, ret) = self.func_types(module, ast, &tp)?;
        let i = self.funcs.len();
        let shown: Vec<String> = args.iter().map(Ty::name).collect();
        self.funcs.push(FuncInfo {
            sig: FuncSig {
                name: format!("{}:{}", ast.name, shown.join(":")),
                params,
                ret,
                exported: ast.exported,
                symbol: format!("orz_f{}_{}_{}", module, ast.name, i),
            },
            ast,
            module,
            tparams: tp,
        });
        self.func_instances.insert(key, i);
        self.pending.push(i);
        Ok(i)
    }

    /// 関数の引数と戻り値の型。
    fn func_types(&mut self, m: usize, f: &Func, tp: &TParams) -> Result<(Vec<Ty>, Option<Ty>), Diag> {
        let mut params = Vec::new();
        for p in &f.params {
            params.push(self.resolve_type(m, &p.ty, tp)?);
        }
        let ret = match &f.ret {
            RetType::Nothing(_) => None,
            RetType::Type(t) => Some(self.resolve_type(m, t, tp)?),
        };
        Ok((params, ret))
    }
}

/// maybe を付けずに自分自身を（ほかの構造体を通しても）含む構造体は、作れないのでエラーにする。
fn check_struct_cycle(ctx: &Ctx, start: usize) -> Result<(), Diag> {
    fn visit(ctx: &Ctx, i: usize, start: usize, seen: &mut HashSet<usize>) -> bool {
        for f in &ctx.structs[i].fields {
            if let Ty::Struct(j, _) = f.ty {
                if j == start {
                    return true;
                }
                if seen.insert(j) && visit(ctx, j, start, seen) {
                    return true;
                }
            }
        }
        false
    }
    if visit(ctx, start, start, &mut HashSet::new()) {
        let def = ctx.structs[start].def;
        return Err(Diag::new(
            def.name_span,
            format!(
                "構造体 `{}` が、maybe を付けずに自分自身を含んでいます（これでは作れません）",
                ctx.structs[start].name
            ),
        )
        .hint("自分と同じ構造体のフィールドには maybe を付けます（例：next:maybe:Node;）"));
    }
    Ok(())
}

fn unknown_type(span: Span, name: &str) -> Diag {
    let d = Diag::new(span, format!("`{}` という型はありません", name));
    match name {
        "int" | "i32" | "i64" | "u8" | "integer" | "long" => d.hint("整数の型は `8`、`32`、`64` です"),
        "float" | "double" | "f64" | "f32" | "real" => d.hint("小数の型は `decnum` です"),
        "str" | "String" | "text" => d.hint("文字列の型は `string` です"),
        "boolean" | "Bool" => d.hint("真偽値の型は `bool` です"),
        "char" => d.hint("1 文字は長さ 1 の `string`、1 バイトは `8` です"),
        _ => d,
    }
}

/// ファイル名から、名前空間の名前を作る（`lib/util.orz` → `util`）。
fn file_stem(path: &str) -> String {
    let base = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let stem = base.strip_suffix(".orz").or_else(|| base.strip_suffix(".orh"));
    stem.unwrap_or(base).to_string()
}

fn is_ident(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// 型引数の型を、引数の型から決める。（`template` の `Ty::Param` を `bind` に入れる）
fn unify(ctx: &Ctx, template: &Ty, actual: &Ty, bind: &mut [Option<Ty>]) {
    match (template, actual) {
        (Ty::Param(i, _), a) if *a != Ty::Null => {
            if bind[*i as usize].is_none() {
                bind[*i as usize] = Some(a.clone());
            }
        }
        (Ty::Pointer(t), Ty::Pointer(a)) | (Ty::Array(t), Ty::Array(a)) | (Ty::Maybe(t), Ty::Maybe(a)) => {
            unify(ctx, t, a, bind)
        }
        // maybe:T の引数に、null でない T の値を渡したとき。
        (Ty::Maybe(t), a) => unify(ctx, t, a, bind),
        (Ty::Func(tr, tps), Ty::Func(ar, aps)) => {
            if let (Some(t), Some(a)) = (tr, ar) {
                unify(ctx, t, a, bind);
            }
            for (t, a) in tps.iter().zip(aps) {
                unify(ctx, t, a, bind);
            }
        }
        (Ty::Struct(ti, _), Ty::Struct(ai, _)) => {
            if let (Some((tg, targs)), Some((ag, aargs))) =
                (&ctx.structs[*ti].origin, &ctx.structs[*ai].origin)
            {
                if tg == ag {
                    for (t, a) in targs.iter().zip(aargs) {
                        unify(ctx, t, a, bind);
                    }
                }
            }
        }
        _ => {}
    }
}

/// 型引数を決まった型に置き換える（構造体の展開はしない。展開済みのものだけ使う）。
fn subst(ctx: &Ctx, t: &Ty, bind: &[Ty]) -> Option<Ty> {
    Some(match t {
        Ty::Param(i, _) => bind.get(*i as usize)?.clone(),
        Ty::Pointer(x) => Ty::Pointer(Box::new(subst(ctx, x, bind)?)),
        Ty::Array(x) => Ty::Array(Box::new(subst(ctx, x, bind)?)),
        Ty::Maybe(x) => Ty::Maybe(Box::new(subst(ctx, x, bind)?)),
        Ty::Func(r, ps) => Ty::Func(
            match r {
                Some(r) => Some(Box::new(subst(ctx, r, bind)?)),
                None => None,
            },
            ps.iter()
                .map(|p| subst(ctx, p, bind))
                .collect::<Option<Vec<_>>>()?,
        ),
        Ty::Struct(i, _)
            if t.contains_param()
                || ctx.structs[*i]
                    .origin
                    .as_ref()
                    .is_some_and(|(_, a)| a.iter().any(Ty::contains_param)) =>
        {
            let (g, args) = ctx.structs[*i].origin.as_ref()?;
            let args = args
                .iter()
                .map(|a| subst(ctx, a, bind))
                .collect::<Option<Vec<_>>>()?;
            let j = *ctx.struct_instances.get(&(*g, args))?;
            ctx.struct_ty(j)
        }
        t => t.clone(),
    })
}

/// `modules[0]` が最初に渡されたファイル。`import` の `resolved` は読み込むときに埋めておく。
pub fn lower(modules: &[Program]) -> Result<ir::Program, Diag> {
    let mut ctx = Ctx {
        mods: Vec::new(),
        funcs: Vec::new(),
        generic_funcs: Vec::new(),
        func_instances: HashMap::new(),
        pending: Vec::new(),
        globals: Vec::new(),
        structs: Vec::new(),
        generic_structs: Vec::new(),
        struct_instances: HashMap::new(),
        strings: Vec::new(),
        string_ids: HashMap::new(),
        global_inits: Vec::new(),
        slot_ids: HashMap::new(),
    };

    // 1. import
    for prog in modules {
        let mut imports = HashMap::new();
        for item in &prog.items {
            let Item::Import(imp) = item else { continue };
            let (r, default_name) = match &imp.target {
                ImportTarget::Std(name) => match name.as_str() {
                    "stdio" => (ModRef::Stdio, name.clone()),
                    // orz で書いた標準ライブラリ（読み込むときに埋めてある）。
                    _ if imp.resolved.is_some() => (ModRef::File(imp.resolved.unwrap()), name.clone()),
                    "math" | "socket" | "http" => {
                        return Err(Diag::new(imp.span, format!("標準ライブラリ `{}` はまだありません", name))
                            .hint("今使えるのは stdio、strings、lists、maps、os、bignum です"))
                    }
                    _ => {
                        return Err(Diag::new(imp.span, format!("標準ライブラリに `{}` はありません", name))
                            .hint("自分のファイルなら `import \"ファイル名.orz\";` と書きます（今使える標準ライブラリは stdio と bignum です）"))
                    }
                },
                ImportTarget::File(path) => {
                    (ModRef::File(imp.resolved.expect("読み込むときに埋めてある")), file_stem(path))
                }
            };
            let ns = imp.alias.clone().unwrap_or(default_name);
            if !is_ident(&ns) {
                return Err(
                    Diag::new(imp.span, format!("`{}` は名前空間の名前に使えません", ns))
                        .hint("`as` で名前を付けてください（例：import \"my-util.orz\" as util;）"),
                );
            }
            if imports.insert(ns.clone(), r).is_some() {
                return Err(
                    Diag::new(imp.span, format!("名前空間 `{}` が 2 回 import されています", ns))
                        .hint("`as` で別の名前を付けられます"),
                );
            }
        }
        ctx.mods.push(ModInfo {
            names: HashMap::new(),
            imports,
        });
    }

    // 2. 名前を集める。関数は定義より前の行からでも呼べる。
    let mut normal_funcs: Vec<(usize, &Func)> = Vec::new();
    for (m, prog) in modules.iter().enumerate() {
        for item in &prog.items {
            let (name, span, top) = match item {
                Item::Func(f) if f.type_params.is_empty() => {
                    (&f.name, f.name_span, TopName::Func(normal_funcs.len()))
                }
                Item::Func(f) if f.is_extern => {
                    return Err(Diag::new(
                        f.name_span,
                        "`extern` の関数はジェネリクスにできません",
                    ))
                }
                Item::Func(f) => (
                    &f.name,
                    f.name_span,
                    TopName::GenericFunc(ctx.generic_funcs.len()),
                ),
                Item::Global(d) => (&d.name, d.name_span, TopName::Global(ctx.globals.len())),
                Item::Struct(s) if s.type_params.is_empty() => {
                    (&s.name, s.name_span, TopName::Struct(ctx.structs.len()))
                }
                Item::Struct(s) => (
                    &s.name,
                    s.name_span,
                    TopName::GenericStruct(ctx.generic_structs.len()),
                ),
                Item::Import(_) => continue,
            };
            let what = match top {
                TopName::Func(_) | TopName::GenericFunc(_) => "関数の名前",
                TopName::Global(_) => "グローバル変数の名前",
                TopName::Struct(_) | TopName::GenericStruct(_) => "構造体の名前",
            };
            ctx.reserved(m, span, name, what)?;
            if ctx.mods[m].names.insert(name, top).is_some() {
                return Err(
                    Diag::new(span, format!("`{}` という名前が 2 回定義されています", name))
                        .hint("関数、構造体、グローバル変数は、すべて違う名前にします"),
                );
            }
            match item {
                Item::Func(f) if f.type_params.is_empty() => normal_funcs.push((m, f)),
                Item::Func(f) => ctx.generic_funcs.push(GenericFunc {
                    ast: f,
                    module: m,
                    params: Vec::new(),
                    ret: None,
                    instances: 0,
                }),
                Item::Global(d) => ctx.globals.push(GlobalInfo {
                    decl: d,
                    module: m,
                    ty: Ty::Null,
                    state: GlobalState::Todo,
                    symbol: format!("orz_g{}_{}", m, d.name),
                }),
                Item::Struct(s) if s.type_params.is_empty() => ctx.structs.push(StructInfo {
                    def: s,
                    module: m,
                    name: Rc::from(s.name.as_str()),
                    fields: Vec::new(),
                    size: 0,
                    origin: None,
                }),
                Item::Struct(s) => ctx.generic_structs.push(GenericStruct {
                    def: s,
                    module: m,
                    instances: 0,
                }),
                Item::Import(_) => {}
            }
        }
    }

    // 3. 構造体のフィールド。ジェネリクスの構造体は、型引数を中身のわからない型にして一度確かめる。
    let nstructs = ctx.structs.len();
    for i in 0..nstructs {
        ctx.resolve_fields(i, &Vec::new())?;
    }
    for g in 0..ctx.generic_structs.len() {
        let def = ctx.generic_structs[g].def;
        let params = opaque_params(&def.type_params);
        ctx.instantiate_struct(g, params, def.name_span)?;
    }

    // 4. 関数の引数と戻り値の型。
    for (m, f) in &normal_funcs {
        check_params(&ctx, *m, f)?;
        let (params, ret) = ctx.func_types(*m, f, &Vec::new())?;
        let symbol = if f.is_extern {
            extern_sig(f, &params, &ret)?;
            f.name.clone()
        } else {
            format!("orz_f{}_{}", m, f.name)
        };
        ctx.funcs.push(FuncInfo {
            sig: FuncSig {
                name: f.name.clone(),
                params,
                ret,
                exported: f.exported,
                symbol,
            },
            ast: f,
            module: *m,
            tparams: Vec::new(),
        });
    }
    for g in 0..ctx.generic_funcs.len() {
        let (f, m) = (ctx.generic_funcs[g].ast, ctx.generic_funcs[g].module);
        check_params(&ctx, m, f)?;
        let tp: TParams = f
            .type_params
            .iter()
            .cloned()
            .zip(opaque_params(&f.type_params))
            .collect();
        let (params, ret) = ctx.func_types(m, f, &tp)?;
        ctx.generic_funcs[g].params = params;
        ctx.generic_funcs[g].ret = ret;
    }

    // 5. グローバル変数の型と初期値（コンパイル時に計算する）。
    for i in 0..ctx.globals.len() {
        let (m, decl) = (ctx.globals[i].module, ctx.globals[i].decl);
        ctx.globals[i].ty = ctx.resolve_type(m, &decl.ty, &Vec::new())?;
    }
    for i in 0..ctx.globals.len() {
        ctx.eval_global(i)?;
    }

    // 6. main（最初に渡されたファイルのもの）
    let main = match ctx.top(0, "main") {
        Some(TopName::Func(i)) => i,
        Some(TopName::GenericFunc(g)) => {
            return Err(Diag::new(
                ctx.generic_funcs[g].ast.name_span,
                "`main` はジェネリクスにできません",
            ))
        }
        Some(TopName::Global(i)) => {
            return Err(Diag::new(
                ctx.globals[i].decl.name_span,
                "`main` は関数の名前に使います",
            ))
        }
        Some(TopName::Struct(i)) => {
            return Err(Diag::new(
                ctx.structs[i].def.name_span,
                "`main` は関数の名前に使います",
            ))
        }
        Some(TopName::GenericStruct(g)) => {
            return Err(Diag::new(
                ctx.generic_structs[g].def.name_span,
                "`main` は関数の名前に使います",
            ))
        }
        None => {
            return Err(Diag::new(Span::new(0, 0), "`main` 関数がありません")
                .hint("プログラムは `function nothing main() { ... }` から始まります"))
        }
    };
    let (main_returns, main_takes_args) = check_main(ctx.funcs[main].ast, &ctx.funcs[main].sig)?;

    // 7. 関数の中身。ジェネリクスの関数は、まず中身のわからない型で確かめてから、使われた型ごとに変換する。
    let mut lowered: Vec<Option<ir::Function>> = Vec::new();
    for i in 0..ctx.funcs.len() {
        lowered.push(Some(lower_func(&mut ctx, i)?));
    }
    for g in 0..ctx.generic_funcs.len() {
        check_generic_func(&mut ctx, g)?;
    }
    while let Some(i) = ctx.pending.pop() {
        let f = lower_func(&mut ctx, i)?;
        if lowered.len() <= i {
            lowered.resize_with(i + 1, || None);
        }
        lowered[i] = Some(f);
    }
    let mut funcs: Vec<ir::Function> = lowered
        .into_iter()
        .map(|f| f.expect("すべての関数を変換済み"))
        .collect();
    // グローバル変数の初期化の関数は、後ろに足す（main の前に、この順に呼ぶ）。
    let mut init_funcs = Vec::new();
    for f in std::mem::take(&mut ctx.global_inits) {
        init_funcs.push(funcs.len() as u32);
        funcs.push(f);
    }

    let struct_descs = ctx
        .structs
        .iter()
        .map(|s| {
            // 名前\0 と、フィールドごとに「名前\0 位置:型の説明\0」、最後に \0。
            let mut d = s.name.as_bytes().to_vec();
            d.push(0);
            for f in &s.fields {
                d.extend_from_slice(f.name.as_bytes());
                d.push(0);
                d.extend_from_slice(format!("{}:{}", f.offset, f.ty.desc()).as_bytes());
                d.push(0);
            }
            d.push(0);
            d
        })
        .collect();
    let globals = ctx
        .globals
        .iter()
        .map(|g| ir::Global {
            symbol: g.symbol.clone(),
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
        struct_descs,
        main: main as u32,
        main_returns,
        main_takes_args,
        init_funcs,
        nslots: ctx.slot_ids.len() as u32,
    })
}

/// 型引数を、中身のわからない型にしたもの。
fn opaque_params(names: &[String]) -> Vec<Ty> {
    names
        .iter()
        .enumerate()
        .map(|(i, n)| Ty::Param(i as u32, Rc::from(n.as_str())))
        .collect()
}

/// `extern` の関数の、C の呼び出し方での引数と戻り値の種類。使えない型ならエラー。
fn extern_sig(f: &Func, params: &[Ty], ret: &Option<Ty>) -> Result<ir::ExternSig, Diag> {
    fn ctype(t: &Ty) -> Option<ir::CType> {
        Some(match t {
            Ty::Int(IntTy::U8) | Ty::Bool => ir::CType::U8,
            Ty::Int(IntTy::I32) => ir::CType::I32,
            Ty::Int(IntTy::I64) | Ty::Address | Ty::Str | Ty::Array(_) | Ty::Pointer(_) | Ty::Struct(..) => {
                ir::CType::Word
            }
            Ty::Dec => ir::CType::Double,
            // null かもしれないアドレス（maybe:string など）。maybe:32 などの箱は C にはわからない。
            Ty::Maybe(inner) if !inner.needs_box() => return ctype(inner),
            _ => return None,
        })
    }
    let bad = |span: Span, t: &Ty| {
        Diag::new(span, format!("`extern` の関数には `{}` 型は使えません", t.name()))
            .hint("使えるのは数、bool、decnum、address、string、array、pointer、構造体と、そのアドレスの maybe です")
    };
    let mut ps = Vec::new();
    for (p, t) in f.params.iter().zip(params) {
        ps.push(ctype(t).ok_or_else(|| bad(p.span, t))?);
    }
    let r = match ret {
        Some(t) => Some(ctype(t).ok_or_else(|| bad(f.ret.span(), t))?),
        None => None,
    };
    Ok(ir::ExternSig { params: ps, ret: r })
}

fn check_params(ctx: &Ctx, m: usize, f: &Func) -> Result<(), Diag> {
    let mut seen = HashSet::new();
    for p in &f.params {
        if !seen.insert(p.name.as_str()) {
            return Err(Diag::new(p.span, format!("引数 `{}` が 2 つあります", p.name)));
        }
        ctx.reserved(m, p.span, &p.name, "引数の名前")?;
        if let Some(top) = ctx.top(m, &p.name) {
            return Err(top_name_conflict(p.span, &p.name, top));
        }
    }
    Ok(())
}

fn top_name_conflict(span: Span, name: &str, top: TopName) -> Diag {
    let what = match top {
        TopName::Func(_) | TopName::GenericFunc(_) => "関数",
        TopName::Global(_) => "グローバル変数",
        TopName::Struct(_) | TopName::GenericStruct(_) => "構造体",
    };
    Diag::new(
        span,
        format!("`{}` は{}の名前なので、変数の名前には使えません", name, what),
    )
    .hint("外側にある名前と同じ名前は付けられません。別の名前にしてください")
}

/// `main` の形を確かめる。（値を返すか、引数を受け取るか）
fn check_main(f: &Func, sig: &FuncSig) -> Result<(bool, bool), Diag> {
    if f.is_extern {
        return Err(Diag::new(f.name_span, "`main` は `extern` にできません"));
    }
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
        let (decl, module) = (self.globals[i].decl, self.globals[i].module);
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
        let mut l = FnLower::new(self, module, None, Mode::Const, Vec::new());
        let (op, vt) = l.expr(&decl.init, Some(&ty))?;
        if !assignable(&ty, &vt) {
            return Err(mismatch(decl.init.span, &ty, &vt));
        }
        if l.needs_init {
            // 配列や構造体：main の前に実行する初期化の関数を作る。
            let op = l.coerce(op, &vt, &ty, decl.init.span)?;
            l.emit(Inst::Store {
                place: Place::Global(i as u32),
                src: op,
            });
            l.emit(Inst::Ret(None));
            let f = l.into_body().into_function(
                format!("グローバル変数 {} の初期化", decl.name),
                format!("orz_init{}_{}", module, decl.name),
                false,
                0,
            );
            self.global_inits.push(f);
            self.globals[i].state = GlobalState::Done(ConstVal::Deferred);
            return Ok(ConstVal::Deferred);
        }
        let mut insts = std::mem::take(&mut l.insts);
        let v = fold::fold_const(&mut insts, op)?.ok_or_else(|| {
            Diag::new(
                decl.init.span,
                "グローバル変数の初期値には、値がコンパイル時に決まる式だけが書けます",
            )
        })?;
        // maybe:32 などに null でない値を入れるときは、箱に入れる。
        let v = match (&ty, v) {
            (Ty::Maybe(inner), ConstVal::Int(x)) if inner.needs_box() && vt != Ty::Null => ConstVal::Boxed(x),
            _ => v,
        };
        self.globals[i].state = GlobalState::Done(v);
        Ok(v)
    }
}

// ---------------- 関数の中 ----------------

/// 何のために変換しているか。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// 普通の関数（と、展開したジェネリクスの関数）。
    Normal,
    /// グローバル変数の初期値（コンパイル時に計算する）。
    Const,
    /// ジェネリクスの関数を、型引数を中身のわからない型にして確かめる（出したものは使わない）。
    Check,
}

struct Local {
    name: String,
    var: u32,
    ty: Ty,
}

struct FnLower<'c, 'p> {
    ctx: &'c mut Ctx<'p>,
    /// 今いるファイル。
    module: usize,
    /// 関数の戻り値の型。`None` は `nothing`。
    ret: Option<Ty>,
    mode: Mode,
    /// ジェネリクスの型引数と、その型。
    tparams: TParams,
    insts: Vec<Inst>,
    ntemps: u32,
    nvars: u32,
    nlabels: u32,
    scopes: Vec<Vec<Local>>,
    /// `while` の（先頭、出口）のラベルと、そのときの arena の深さ。
    loops: Vec<(u32, u32, u32)>,
    /// 今の arena の深さ（arena の外は 0）。
    arena_depth: u32,
    /// 命令ごとの、もとの文の場所。
    spans: Vec<Span>,
    cur_span: Span,
    var_depth: Vec<u32>,
    ptr_temps: Vec<bool>,
    ptr_vars: Vec<bool>,
    /// 命令ごとの、読み書きする入れ場所の種類（`ir::Function::slots`）。
    slots: Vec<Vec<u32>>,
    /// 関数の中で宣言される名前すべて（エラーメッセージ用）。
    declared_somewhere: HashSet<String>,
    /// 今の場所で null でないとわかっている `maybe` の変数。
    narrowed: HashSet<u32>,
    /// 今の場所に来ることがあるか（`return` の後ろなどは来ない）。
    reachable: bool,
    /// グローバル変数の初期値で、main の前に実行する必要がある（配列や構造体を作る）か。
    needs_init: bool,
}

fn lower_func(ctx: &mut Ctx, index: usize) -> Result<ir::Function, Diag> {
    let (ast, module, tparams, sig) = {
        let f = &ctx.funcs[index];
        (f.ast, f.module, f.tparams.clone(), f.sig.clone())
    };
    if ast.is_extern {
        return Ok(ir::Function {
            name: sig.name.clone(),
            symbol: sig.symbol.clone(),
            returns_value: sig.ret.is_some(),
            nparams: sig.params.len() as u32,
            insts: Vec::new(),
            ntemps: 0,
            nvars: 0,
            spans: Vec::new(),
            var_depth: Vec::new(),
            ptr_temps: Vec::new(),
            ptr_vars: Vec::new(),
            slots: Vec::new(),
            ext: Some(extern_sig(ast, &sig.params, &sig.ret)?),
        });
    }
    let l = lower_body(
        ctx,
        module,
        ast,
        &sig.params,
        sig.ret.clone(),
        Mode::Normal,
        tparams,
    )?;
    Ok(l.into_function(sig.name, sig.symbol, sig.ret.is_some(), sig.params.len() as u32))
}

/// ジェネリクスの関数を、型引数を中身のわからない型にして確かめる。
fn check_generic_func(ctx: &mut Ctx, g: usize) -> Result<(), Diag> {
    let gf = &ctx.generic_funcs[g];
    let (ast, module, params, ret) = (gf.ast, gf.module, gf.params.clone(), gf.ret.clone());
    let tp: TParams = ast
        .type_params
        .iter()
        .cloned()
        .zip(opaque_params(&ast.type_params))
        .collect();
    lower_body(ctx, module, ast, &params, ret, Mode::Check, tp)?;
    Ok(())
}

/// 関数の中身を変換したもの。
struct Body {
    insts: Vec<Inst>,
    ntemps: u32,
    nvars: u32,
    spans: Vec<Span>,
    var_depth: Vec<u32>,
    ptr_temps: Vec<bool>,
    ptr_vars: Vec<bool>,
    slots: Vec<Vec<u32>>,
}

impl Body {
    fn into_function(self, name: String, symbol: String, returns_value: bool, nparams: u32) -> ir::Function {
        ir::Function {
            name,
            symbol,
            returns_value,
            nparams,
            insts: self.insts,
            ntemps: self.ntemps,
            nvars: self.nvars,
            spans: self.spans,
            var_depth: self.var_depth,
            ptr_temps: self.ptr_temps,
            ptr_vars: self.ptr_vars,
            slots: self.slots,
            ext: None,
        }
    }
}

/// 関数の中身を変換する。
fn lower_body(
    ctx: &mut Ctx,
    module: usize,
    f: &Func,
    params: &[Ty],
    ret: Option<Ty>,
    mode: Mode,
    tparams: TParams,
) -> Result<Body, Diag> {
    let returns = ret.is_some();
    let mut l = FnLower::new(ctx, module, ret, mode, tparams);
    collect_decls(&f.body, &mut l.declared_somewhere);
    l.scopes.push(Vec::new());
    for (p, ty) in f.params.iter().zip(params) {
        let var = l.new_var(ty.has_level());
        l.scopes[0].push(Local {
            name: p.name.clone(),
            var,
            ty: ty.clone(),
        });
    }
    l.block(&f.body)?;
    if returns {
        if !block_returns(&f.body) {
            return Err(Diag::new(
                f.body.close,
                format!("関数 `{}` が値を返さずに終わることがあります", f.name),
            )
            .hint("どの道を通っても `return 値;` に着くようにしてください"));
        }
    } else {
        l.emit(Inst::Ret(None));
    }
    Ok(l.into_body())
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

const MAYBE_HINT: &str = "null かもしれない値は、`if (x != null) { … }` で確かめると、その中で使えます。確かめられるのはローカル変数と引数だけなので、フィールド・配列の要素・グローバル変数は、いったんローカル変数に入れてから確かめます";

const PARAM_HINT: &str =
    "型引数（T など）の値にできるのは、代入、引数として渡す、return だけです。計算や比較が必要なら、その関数を引数で受け取ります（例：less:function:bool(T, T)）";

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
        (w, Ty::Maybe(inner)) if **inner == *w => d.hint(MAYBE_HINT),
        (_, Ty::Null) => d.hint("`null` は `maybe` の付いた型にだけ入れられます"),
        (Ty::Bool, Ty::Int(_)) => d.hint("整数を条件に使うなら、`x != 0` のように比べてください"),
        (Ty::Str, Ty::Int(_) | Ty::Bool | Ty::Address | Ty::Dec) => {
            d.hint("文字列にするなら `to:string(…)` を使います")
        }
        (Ty::Int(a), Ty::Int(_) | Ty::Dec) => d.hint(format!(
            "型の違う数は混ぜられません。`to:{}(…)` で変換してください",
            a.name()
        )),
        (Ty::Dec, Ty::Int(_)) => d.hint("型の違う数は混ぜられません。`to:decnum(…)` で変換してください"),
        (Ty::Func(..), Ty::Func(..)) => d.hint("関数の引数と戻り値の型が、すべて同じでなければなりません"),
        _ => d,
    }
}

fn maybe_hint(d: Diag) -> Diag {
    d.hint(MAYBE_HINT)
}

impl<'c, 'p> FnLower<'c, 'p> {
    fn new(ctx: &'c mut Ctx<'p>, module: usize, ret: Option<Ty>, mode: Mode, tparams: TParams) -> Self {
        FnLower {
            ctx,
            module,
            ret,
            mode,
            tparams,
            insts: Vec::new(),
            ntemps: 0,
            nvars: 0,
            nlabels: 0,
            scopes: Vec::new(),
            loops: Vec::new(),
            declared_somewhere: HashSet::new(),
            narrowed: HashSet::new(),
            reachable: true,
            needs_init: false,
            arena_depth: 0,
            spans: Vec::new(),
            cur_span: Span::default(),
            var_depth: Vec::new(),
            ptr_temps: Vec::new(),
            ptr_vars: Vec::new(),
            slots: Vec::new(),
        }
    }

    fn into_body(self) -> Body {
        Body {
            insts: self.insts,
            ntemps: self.ntemps,
            nvars: self.nvars,
            spans: self.spans,
            var_depth: self.var_depth,
            ptr_temps: self.ptr_temps,
            ptr_vars: self.ptr_vars,
            slots: self.slots,
        }
    }

    fn const_ctx(&self) -> bool {
        self.mode == Mode::Const
    }

    fn temp(&mut self) -> u32 {
        self.ntemps += 1;
        self.ptr_temps.push(false);
        self.ntemps - 1
    }

    /// t 型の値を入れる一時変数。
    fn ptr_temp(&mut self, t: &Ty) -> u32 {
        let dst = self.temp();
        self.ptr_temps[dst as usize] = t.has_level();
        dst
    }

    /// 変数を作る。`ptr` は、どこかで作った値のアドレスを持つか。
    fn new_var(&mut self, ptr: bool) -> u32 {
        self.nvars += 1;
        self.var_depth.push(self.arena_depth);
        self.ptr_vars.push(ptr);
        self.nvars - 1
    }

    fn label(&mut self) -> u32 {
        self.nlabels += 1;
        self.nlabels - 1
    }

    fn emit(&mut self, i: Inst) {
        self.insts.push(i);
        self.spans.push(self.cur_span);
        self.slots.push(Vec::new());
    }

    /// 命令を、読み書きする入れ場所の種類と一緒に出す。
    fn emit_slots(&mut self, i: Inst, slots: Vec<u32>) {
        self.emit(i);
        *self.slots.last_mut().unwrap() = slots;
    }

    fn top(&self, name: &str) -> Option<TopName> {
        self.ctx.top(self.module, name)
    }

    fn resolve_type(&mut self, t: &TypeExpr) -> Result<Ty, Diag> {
        let tp = self.tparams.clone();
        self.ctx.resolve_type(self.module, t, &tp)
    }

    fn is_namespace(&self, name: &str) -> bool {
        self.lookup(name).is_none() && self.ctx.mods[self.module].imports.contains_key(name)
    }

    /// ランタイムの関数を呼ぶ。グローバル変数の初期値では呼べない。
    fn rt(&mut self, func: Rt, args: Vec<Operand>, span: Span, returns: bool) -> Result<Option<u32>, Diag> {
        if self.const_ctx() {
            // 初期値では、値を作るだけのもの（配列・構造体・文字列）は使える。main の前に実行する。
            let pure = matches!(
                func,
                Rt::Alloc
                    | Rt::ArrayNew
                    | Rt::Concat
                    | Rt::StrFromInt
                    | Rt::StrFromBool
                    | Rt::StrFromDec
                    | Rt::StrToInt
                    | Rt::StrToDec
                    | Rt::Box
                    | Rt::Copy
                    | Rt::ArrayCopy
                    | Rt::ArrayFill
                    | Rt::Pow(_)
            );
            if !pure {
                return Err(Diag::new(
                    span,
                    "グローバル変数の初期値には、値がコンパイル時に決まる式だけが書けます",
                )
                .hint("表示や入出力は、関数の中で行います"));
            }
            self.needs_init = true;
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

    fn rt_value(&mut self, func: Rt, args: Vec<Operand>, span: Span) -> Result<Operand, Diag> {
        Ok(Operand::Temp(self.rt(func, args, span, true)?.unwrap()))
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
        self.ctx.reserved(self.module, span, name, "変数の名前")?;
        if let Some(top) = self.top(name) {
            return Err(top_name_conflict(span, name, top));
        }
        let var = self.new_var(ty.has_level());
        self.scopes.last_mut().unwrap().push(Local {
            name: name.to_string(),
            var,
            ty,
        });
        Ok(var)
    }

    /// `from` 型の値を `to` 型の場所に入れる形にする（maybe:32 などなら箱に入れる）。
    fn coerce(&mut self, v: Operand, from: &Ty, to: &Ty, span: Span) -> Result<Operand, Diag> {
        match to {
            Ty::Maybe(inner) if inner.needs_box() && **inner == *from => {
                self.rt_value(Rt::Box, vec![v], span)
            }
            _ => Ok(v),
        }
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
        self.cur_span = s.span;
        match &s.kind {
            StmtKind::Decl(d) => {
                let ty = self.resolve_type(&d.ty)?;
                // 初期値の中では、まだ自分自身は使えない。
                let (v, vt) = self.expr_to(&d.init, &ty)?;
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
                let Some(&(head, end, depth)) = self.loops.last() else {
                    let kw = if is_break { "break" } else { "continue" };
                    return Err(Diag::new(
                        s.span,
                        format!("`{}` は `while` の中でだけ使えます", kw),
                    ));
                };
                // while の中にある arena から抜けるなら、その分を返す。
                if self.arena_depth > depth {
                    self.emit(Inst::ArenaPop {
                        n: self.arena_depth - depth,
                    });
                }
                self.emit(Inst::Jump(if is_break { end } else { head }));
                self.reachable = false;
            }
            StmtKind::Return(None) => {
                if self.ret.is_some() {
                    return Err(Diag::new(s.span, "値を返す関数なので、`return` に値が必要です"));
                }
                if self.arena_depth > 0 {
                    self.emit(Inst::ArenaPop { n: self.arena_depth });
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
                if self.arena_depth > 0 {
                    self.emit(Inst::ArenaPop { n: self.arena_depth });
                }
                self.emit(Inst::Ret(Some(v)));
                self.reachable = false;
            }
            StmtKind::Arena(b) => {
                if self.arena_depth >= MAX_ARENA_DEPTH {
                    return Err(Diag::new(
                        s.span,
                        format!(
                            "arena の入れ子が深すぎます（1 つの関数の中で {} 段まで）",
                            MAX_ARENA_DEPTH
                        ),
                    ));
                }
                self.emit(Inst::ArenaEnter);
                self.arena_depth += 1;
                self.block(b)?;
                self.arena_depth -= 1;
                self.cur_span = b.close;
                self.emit(Inst::ArenaExit);
            }
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

    /// グローバル変数への代入先。immutable なら代入できない。
    fn global_place(&self, i: usize, span: Span, name: &str) -> Result<(Place, Ty), Diag> {
        let g = &self.ctx.globals[i];
        if g.decl.immutable {
            return Err(Diag::new(
                span,
                format!("`{}` は immutable なので、値を変えられません", name),
            ));
        }
        Ok((Place::Global(i as u32), g.ty.clone()))
    }

    fn assign(&mut self, target: &Expr, value: &Expr) -> Result<(), Diag> {
        let (place, ty, local) = match &target.kind {
            ExprKind::Var(name) => {
                if let Some(l) = self.lookup(name) {
                    (Place::Var(l.var), l.ty.clone(), Some(l.var))
                } else {
                    match self.top(name) {
                        Some(TopName::Global(i)) => {
                            let (p, t) = self.global_place(i, target.span, name)?;
                            (p, t, None)
                        }
                        Some(TopName::Func(_) | TopName::GenericFunc(_)) => {
                            return Err(Diag::new(
                                target.span,
                                format!("`{}` は関数なので、代入できません", name),
                            ))
                        }
                        Some(TopName::Struct(_) | TopName::GenericStruct(_)) => {
                            return Err(Diag::new(
                                target.span,
                                format!("`{}` は構造体なので、代入できません", name),
                            ))
                        }
                        None => return Err(self.unknown_var(target.span, name)),
                    }
                }
            }
            ExprKind::Field(obj, f) => {
                if let ExprKind::Var(ns) = &obj.kind {
                    if self.is_namespace(ns) {
                        let (p, t) = match self.ctx.member(self.module, ns, f, target.span)? {
                            Target::Top(TopName::Global(i)) => {
                                self.global_place(i, target.span, &format!("{}.{}", ns, f))?
                            }
                            _ => {
                                return Err(Diag::new(
                                    target.span,
                                    format!("`{}.{}` には代入できません", ns, f),
                                ))
                            }
                        };
                        let v = self.expr_as(value, &t)?;
                        self.emit(Inst::Store { place: p, src: v });
                        return Ok(());
                    }
                }
                let (addr, fty, slots) = self.field_addr(target, obj, f)?;
                let v = self.expr_as(value, &fty)?;
                self.emit_slots(
                    Inst::StoreMem {
                        addr,
                        src: v,
                        width: fty.width(),
                    },
                    slots,
                );
                return Ok(());
            }
            ExprKind::Index(base, index) => {
                let (addr, elem, slots) = self.elem_addr(target, base, index)?;
                let v = self.expr_as(value, &elem)?;
                self.emit_slots(
                    Inst::StoreMem {
                        addr,
                        src: v,
                        width: elem.width(),
                    },
                    slots,
                );
                return Ok(());
            }
            _ => {
                return Err(Diag::new(target.span, "ここには代入できません")
                    .hint("`=` の左には変数、`a[i]`、`p.x` を書きます。比べるなら `==` です"))
            }
        };
        let (v, vt) = self.expr_to(value, &ty)?;
        self.emit(Inst::Store { place, src: v });
        if let Some(var) = local {
            self.after_store(var, &ty, &vt);
        }
        Ok(())
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
        self.loops.push((head, end, self.arena_depth));
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

    /// 式を `ty` 型の場所に入れる値として計算する。（入れる形にした値, 式そのものの型）
    fn expr_to(&mut self, e: &Expr, ty: &Ty) -> Result<(Operand, Ty), Diag> {
        let (v, t) = self.expr(e, Some(ty))?;
        if !assignable(ty, &t) {
            return Err(mismatch(e.span, ty, &t));
        }
        let v = self.coerce(v, &t, ty, e.span)?;
        Ok((v, t))
    }

    fn expr_as(&mut self, e: &Expr, ty: &Ty) -> Result<Operand, Diag> {
        Ok(self.expr_to(e, ty)?.0)
    }

    /// 呼び出す関数の名前が指すもの（`f`、`ns.f`）。ローカル変数なら `None`。
    fn callee_target(&self, callee: &Expr) -> Option<(Target, String)> {
        match &callee.kind {
            ExprKind::Var(name) if self.lookup(name).is_none() => {
                self.top(name).map(|t| (Target::Top(t), name.clone()))
            }
            ExprKind::Field(obj, f) => match &obj.kind {
                ExprKind::Var(ns) if self.is_namespace(ns) => self
                    .ctx
                    .member(self.module, ns, f, callee.span)
                    .ok()
                    .map(|t| (t, format!("{}.{}", ns, f))),
                _ => None,
            },
            _ => None,
        }
    }

    /// ジェネリクスの関数を呼んだときの戻り値の型を、引数から調べる（展開はしない）。
    fn peek_generic_ret(&self, g: usize, explicit: Option<Vec<Ty>>, args: &[Expr]) -> Option<Ty> {
        let gf = &self.ctx.generic_funcs[g];
        let bind: Vec<Ty> = match explicit {
            Some(b) => b,
            None => {
                let mut bind = vec![None; gf.ast.type_params.len()];
                for (arg, t) in args.iter().zip(&gf.params) {
                    if let Some(at) = self.peek(arg) {
                        unify(self.ctx, t, &at, &mut bind);
                    }
                }
                bind.into_iter().collect::<Option<Vec<_>>>()?
            }
        };
        subst(self.ctx, gf.ret.as_ref()?, &bind)
    }

    /// 式の型を、計算せずに調べる。整数リテラルだけの式など、文脈で決まるものは `None`。
    fn peek(&self, e: &Expr) -> Option<Ty> {
        match &e.kind {
            ExprKind::Bool(_) => Some(Ty::Bool),
            ExprKind::Str(_) => Some(Ty::Str),
            ExprKind::Char(_) => Some(Ty::Int(IntTy::U8)),
            ExprKind::Float(_) => Some(Ty::Dec),
            ExprKind::Null => Some(Ty::Null),
            ExprKind::Var(name) => {
                if let Some(l) = self.lookup(name) {
                    return Some(self.local_type(l));
                }
                match self.top(name)? {
                    TopName::Global(i) => Some(self.ctx.globals[i].ty.clone()),
                    TopName::Func(i) => Some(self.ctx.funcs[i].sig.ty()),
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
            ExprKind::Call(callee, args) => {
                if let ExprKind::Var(name) = &callee.kind {
                    if self.lookup(name).is_none() {
                        match name.as_str() {
                            "length" => return Some(I32),
                            "bytes" => return Some(Ty::Pointer(Box::new(Ty::Int(IntTy::U8)))),
                            "copy" => return args.first().and_then(|a| self.peek(a)),
                            _ => {}
                        }
                    }
                }
                if let ExprKind::Field(obj, f) = &callee.kind {
                    if let ExprKind::Var(ns) = &obj.kind {
                        if self.is_namespace(ns) {
                            if let Ok(Target::Stdio) = self.ctx.member(self.module, ns, f, e.span) {
                                return match f.as_str() {
                                    "read_line" | "read_file" => Some(Ty::Maybe(Box::new(Ty::Str))),
                                    "write_file" => Some(Ty::Bool),
                                    "last_error" => Some(Ty::Str),
                                    _ => None,
                                };
                            }
                        }
                    }
                }
                if let ExprKind::Generic(..) = &callee.kind {
                    // 型を書いたジェネリクスの呼び出しは、型を展開しないとわからないので調べない。
                    return None;
                }
                match self.callee_target(callee) {
                    Some((Target::Top(TopName::Func(i)), _)) => self.ctx.funcs[i].sig.ret.clone(),
                    Some((Target::Top(TopName::Struct(i)), _)) => Some(self.ctx.struct_ty(i)),
                    Some((Target::Top(TopName::GenericFunc(g)), _)) => self.peek_generic_ret(g, None, args),
                    Some(_) => None,
                    None => match self.peek(callee)? {
                        Ty::Func(ret, _) => ret.map(|t| *t),
                        _ => None,
                    },
                }
            }
            ExprKind::Field(obj, f) => match &obj.kind {
                ExprKind::Var(ns) if self.is_namespace(ns) => {
                    match self.ctx.member(self.module, ns, f, e.span) {
                        Ok(Target::Top(TopName::Global(i))) => Some(self.ctx.globals[i].ty.clone()),
                        Ok(Target::Top(TopName::Func(i))) => Some(self.ctx.funcs[i].sig.ty()),
                        _ => None,
                    }
                }
                _ => match self.peek(obj)? {
                    Ty::Struct(i, _) => self.ctx.structs[i]
                        .fields
                        .iter()
                        .find(|x| x.name == *f)
                        .map(|x| x.ty.clone()),
                    _ => None,
                },
            },
            ExprKind::Index(base, _) => match self.peek(base)? {
                Ty::Array(t) | Ty::Pointer(t) => Some(*t),
                _ => None,
            },
            ExprKind::Convert(t, x) => {
                // 型の名前を調べるだけ（ジェネリクスの構造体は展開済みのものだけ）。
                let to = self.peek_type(t)?;
                // 文字列から数への変換は、失敗することがあるので maybe になる。
                if to.is_numeric() && to != Ty::Address && self.peek(x) == Some(Ty::Str) {
                    Some(Ty::Maybe(Box::new(to)))
                } else {
                    Some(to)
                }
            }
            ExprKind::ArrayLit(elems) => {
                let t = elems.first().and_then(|x| self.peek(x))?;
                Some(Ty::Array(Box::new(t)))
            }
            _ => None,
        }
    }

    /// 型を、展開をしないで調べる（`peek` 用。基本の型だけ）。
    fn peek_type(&self, t: &TypeExpr) -> Option<Ty> {
        match t.atoms.as_slice() {
            [a] => match &a.kind {
                AtomKind::Int(it) => Some(Ty::Int(*it)),
                AtomKind::Bool => Some(Ty::Bool),
                AtomKind::Decnum => Some(Ty::Dec),
                AtomKind::String => Some(Ty::Str),
                AtomKind::Address => Some(Ty::Address),
                _ => None,
            },
            _ => None,
        }
    }

    /// 式を計算する。`want` は文脈から期待される型（整数リテラルの型などを決めるのに使う）。
    fn expr(&mut self, e: &Expr, want: Option<&Ty>) -> Result<(Operand, Ty), Diag> {
        let (v, t) = self.expr_inner(e, want)?;
        // arena の持ち出しの検査のため、アドレスを持つ一時変数に印を付ける。
        if let Operand::Temp(i) = v {
            if t.has_level() {
                self.ptr_temps[i as usize] = true;
            }
        }
        Ok((v, t))
    }

    fn expr_inner(&mut self, e: &Expr, want: Option<&Ty>) -> Result<(Operand, Ty), Diag> {
        match &e.kind {
            ExprKind::Int(n) => int_literal(*n, false, want, e.span),
            ExprKind::Float(f) => dec_literal(*f, false, e.span),
            ExprKind::Char(c) => Ok((Operand::Imm(*c as i64), Ty::Int(IntTy::U8))),
            ExprKind::Bool(b) => Ok((Operand::Imm(*b as i64), Ty::Bool)),
            ExprKind::Str(s) => Ok((Operand::Str(self.ctx.intern(s)), Ty::Str)),
            ExprKind::Null => Ok((Operand::Imm(0), Ty::Null)),
            ExprKind::Var(name) => self.var(e.span, name, want),
            ExprKind::Unary(UnOp::Neg, inner) => {
                // `-2147483648` は、2147483648 が 32 に入らなくても書けるようにする。
                match inner.kind {
                    ExprKind::Int(n) => return int_literal(n, true, want, e.span),
                    ExprKind::Float(f) => return dec_literal(f, true, e.span),
                    _ => {}
                }
                let (a, t) = self.expr(inner, want)?;
                let dst = self.temp();
                if t == Ty::Dec {
                    self.emit(Inst::FNeg { dst, a });
                    return Ok((Operand::Temp(dst), t));
                }
                let Some(it) = t.int_like() else {
                    let d = Diag::new(e.span, format!("符号の `-` は `{}` には使えません", t.name()));
                    return Err(match t {
                        Ty::Bool => d.hint("真と偽を入れ替えるなら `not` を使います"),
                        Ty::Param(..) => d.hint(PARAM_HINT),
                        _ => d,
                    });
                };
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
                match (&t, t.int_like()) {
                    (Ty::Bool, _) => self.emit(Inst::BoolNot { dst, a }),
                    (_, Some(it)) => self.emit(Inst::Not { dst, ty: it, a }),
                    _ => {
                        let d = Diag::new(
                            e.span,
                            format!(
                                "`not` は `bool` と整数にだけ使えますが、この式は `{}` 型です",
                                t.name()
                            ),
                        );
                        return Err(if t.contains_param() { d.hint(PARAM_HINT) } else { d });
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
                let (addr, elem, slots) = self.elem_addr(e, base, index)?;
                let dst = self.ptr_temp(&elem);
                self.emit_slots(
                    Inst::Load {
                        dst,
                        addr,
                        width: elem.width(),
                    },
                    slots,
                );
                Ok((Operand::Temp(dst), elem))
            }
            ExprKind::Field(obj, f) => {
                if let ExprKind::Var(ns) = &obj.kind {
                    if self.is_namespace(ns) {
                        let shown = format!("{}.{}", ns, f);
                        return match self.ctx.member(self.module, ns, f, e.span)? {
                            Target::Top(top) => self.top_value(e.span, top, &shown, want),
                            Target::Stdio => Err(Diag::new(
                                e.span,
                                "名前空間の関数は、`stdio.print(…)` のように呼び出しの形で使います",
                            )),
                        };
                    }
                }
                let (addr, fty, slots) = self.field_addr(e, obj, f)?;
                let dst = self.ptr_temp(&fty);
                self.emit_slots(
                    Inst::Load {
                        dst,
                        addr,
                        width: fty.width(),
                    },
                    slots,
                );
                Ok((Operand::Temp(dst), fty))
            }
            ExprKind::Generic(inner, types) => {
                // `less:32` のように型を書いた関数は、値（関数ポインタ）として使える。
                let Some((target, shown)) = self.callee_target(inner) else {
                    return Err(Diag::new(
                        e.span,
                        "型を付けられるのは、ジェネリクスの関数と構造体の名前だけです",
                    ));
                };
                match target {
                    Target::Top(TopName::GenericFunc(g)) => {
                        let n = self.ctx.generic_funcs[g].ast.type_params.len();
                        let args = self.type_args(types, n, &shown)?;
                        self.generic_func_value(e.span, g, args)
                    }
                    Target::Top(TopName::GenericStruct(_)) => Err(Diag::new(
                        e.span,
                        format!(
                            "`{}` は構造体です。作るなら `{}:…(…)` のように値を渡します",
                            shown, shown
                        ),
                    )),
                    _ => Err(Diag::new(
                        e.span,
                        format!("`{}` はジェネリクスではないので、型は付けられません", shown),
                    )),
                }
            }
            ExprKind::Convert(ty, x) => self.convert(e, ty, x),
            ExprKind::SizeOf(ty) => {
                let t = self.resolve_type(ty)?;
                // 構造体の変数や pointer:構造体 の要素に入るのはアドレスなので、構造体も 8 バイト。
                int_literal(t.width().bytes() as u64, false, want, e.span)
            }
            ExprKind::ArrayLit(elems) => self.array_lit(e, elems, want),
            ExprKind::ArrayNew(n, init) => self.array_new(e, n, init.as_deref(), want),
        }
    }

    /// 型引数を `n` 個読む（`get:32:string` の `32:string`）。
    fn type_args(&mut self, types: &TypeExpr, n: usize, shown: &str) -> Result<Vec<Ty>, Diag> {
        let tp = self.tparams.clone();
        let mut pos = 0;
        let mut args = Vec::new();
        for _ in 0..n {
            if pos >= types.atoms.len() {
                return Err(Diag::new(
                    types.span,
                    format!("`{}` には型を {} つ付けます", shown, n),
                ));
            }
            args.push(self.ctx.resolve_atoms(self.module, &types.atoms, &mut pos, &tp)?);
        }
        if pos < types.atoms.len() {
            return Err(Diag::new(
                types.atoms[pos].span,
                format!("`{}` に付ける型は {} つです", shown, n),
            ));
        }
        Ok(args)
    }

    /// ジェネリクスの関数を、型を決めて値（関数ポインタ）にする。
    fn generic_func_value(&mut self, span: Span, g: usize, args: Vec<Ty>) -> Result<(Operand, Ty), Diag> {
        if self.mode == Mode::Check || args.iter().any(Ty::contains_param) {
            // 確かめるだけのときは展開しない（型だけ求める）。
            let gf = &self.ctx.generic_funcs[g];
            let (ret, params) = (gf.ret.clone(), gf.params.clone());
            let ret = ret.map(|r| subst(self.ctx, &r, &args)).unwrap_or(None);
            let params = params
                .iter()
                .map(|p| subst(self.ctx, p, &args))
                .collect::<Option<Vec<_>>>();
            let ty = Ty::Func(ret.map(Box::new), params.unwrap_or_default());
            return Ok((Operand::Imm(0), ty));
        }
        let i = self.ctx.instantiate_func(g, args, span)?;
        Ok((Operand::Func(i as u32), self.ctx.funcs[i].sig.ty()))
    }

    /// 名前（関数・グローバル変数・構造体）を値として使う。
    fn top_value(
        &mut self,
        span: Span,
        top: TopName,
        name: &str,
        want: Option<&Ty>,
    ) -> Result<(Operand, Ty), Diag> {
        match top {
            TopName::Global(i) => self.global_read(i, span, name),
            TopName::Func(i) => {
                if self.const_ctx() {
                    return Err(Diag::new(span, "グローバル変数の初期値に、関数は使えません"));
                }
                if self.ctx.funcs[i].ast.is_extern {
                    return Err(Diag::new(
                        span,
                        format!("`extern` の関数 `{}` は、関数ポインタにはできません", name),
                    )
                    .hint("呼び出し方が orz の関数と違うためです。orz の関数で包んでから使います"));
                }
                Ok((Operand::Func(i as u32), self.ctx.funcs[i].sig.ty()))
            }
            TopName::GenericFunc(g) => {
                // 入れる先の関数の型から、型引数を決める。
                let template = {
                    let gf = &self.ctx.generic_funcs[g];
                    Ty::Func(gf.ret.clone().map(Box::new), gf.params.clone())
                };
                let n = self.ctx.generic_funcs[g].ast.type_params.len();
                let mut bind = vec![None; n];
                if let Some(w @ Ty::Func(..)) = want.map(|w| match w {
                    Ty::Maybe(inner) => &**inner,
                    w => w,
                }) {
                    unify(self.ctx, &template, w, &mut bind);
                }
                let Some(args) = bind.into_iter().collect::<Option<Vec<_>>>() else {
                    return Err(Diag::new(
                        span,
                        format!("ジェネリクスの関数 `{}` の型引数が決まりません", name),
                    )
                    .hint(format!("`{}:32` のように型を書きます", name)));
                };
                self.generic_func_value(span, g, args)
            }
            TopName::Struct(_) | TopName::GenericStruct(_) => Err(Diag::new(
                span,
                format!("`{}` は構造体の名前です。作るなら `{}(…)` と書きます", name, name),
            )),
        }
    }

    fn global_read(&mut self, i: usize, span: Span, name: &str) -> Result<(Operand, Ty), Diag> {
        let ty = self.ctx.globals[i].ty.clone();
        let immutable = self.ctx.globals[i].decl.immutable;
        if immutable {
            // immutable の値はコンパイル時に決まっているので、そのまま埋め込む。
            match self.ctx.eval_global(i)? {
                ConstVal::Int(v) => return Ok((Operand::Imm(v), ty)),
                ConstVal::Str(s) => return Ok((Operand::Str(s), ty)),
                // 箱に入った値や、初期化の関数で入れる値は、グローバル変数の場所から読む。
                ConstVal::Boxed(_) | ConstVal::Deferred => {
                    if self.const_ctx() {
                        // ほかのグローバル変数の初期化の中なら、それも main の前に実行する。
                        self.needs_init = true;
                        let dst = self.temp();
                        self.emit(Inst::Copy {
                            dst,
                            src: Operand::Global(i as u32),
                        });
                        return Ok((Operand::Temp(dst), ty));
                    }
                }
            }
        } else if self.const_ctx() {
            return Err(Diag::new(
                span,
                format!(
                    "値の変わるグローバル変数 `{}` は、グローバル変数の初期値には使えません",
                    name
                ),
            )
            .hint(format!("`{}` に `immutable` を付けると使えます", name)));
        }
        if self.const_ctx() {
            return Err(Diag::new(
                span,
                "グローバル変数の初期値には、値がコンパイル時に決まる式だけが書けます",
            ));
        }
        // 読んだ時点の値を一時変数に写す（後の関数呼び出しで書き換わっても影響しないように）。
        let dst = self.temp();
        self.emit(Inst::Copy {
            dst,
            src: Operand::Global(i as u32),
        });
        Ok((Operand::Temp(dst), ty))
    }

    fn var(&mut self, span: Span, name: &str, want: Option<&Ty>) -> Result<(Operand, Ty), Diag> {
        if let Some(l) = self.lookup(name) {
            let (var, declared, ty) = (l.var, l.ty.clone(), self.local_type(l));
            let dst = self.temp();
            self.emit(Inst::Copy {
                dst,
                src: Operand::Var(var),
            });
            // 箱に入った maybe:32 などは、null でないとわかっていれば中身を取り出す。
            if declared.is_maybe() && !ty.is_maybe() && ty.needs_box() {
                let v = self.temp();
                self.emit(Inst::Load {
                    dst: v,
                    addr: Operand::Temp(dst),
                    width: Width::W8,
                });
                return Ok((Operand::Temp(v), ty));
            }
            return Ok((Operand::Temp(dst), ty));
        }
        match self.top(name) {
            Some(top) => self.top_value(span, top, name, want),
            None if self.ctx.mods[self.module].imports.contains_key(name) => Err(Diag::new(
                span,
                format!("`{}` は名前空間です。`{}.名前` のように使います", name, name),
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
    /// `a[i]` の要素のアドレス。（アドレス, 要素の型, 入れ場所の種類）
    fn elem_addr(&mut self, e: &Expr, base: &Expr, index: &Expr) -> Result<(Operand, Ty, Vec<u32>), Diag> {
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
        let slots = self.ctx.elem_slots(&elem);
        Ok((Operand::Temp(dst), elem, slots))
    }

    /// `p.x` のフィールドのアドレス。（アドレス, フィールドの型, 入れ場所の種類）
    fn field_addr(&mut self, e: &Expr, obj: &Expr, field: &str) -> Result<(Operand, Ty, Vec<u32>), Diag> {
        let (ov, ot) = self.expr(obj, None)?;
        let i = match &ot {
            Ty::Struct(i, _) => *i,
            Ty::Maybe(inner) if matches!(**inner, Ty::Struct(..)) => {
                return Err(maybe_hint(Diag::new(
                    obj.span,
                    format!(
                        "`{}` 型の値には、null でないことを確かめるまで `.` は使えません",
                        ot.name()
                    ),
                )))
            }
            _ => {
                return Err(Diag::new(
                    obj.span,
                    format!(
                        "`.` でフィールドを使えるのは構造体だけですが、この式は `{}` 型です",
                        ot.name()
                    ),
                ))
            }
        };
        let Some(j) = self.ctx.structs[i].fields.iter().position(|f| f.name == field) else {
            let names: Vec<&str> = self.ctx.structs[i]
                .fields
                .iter()
                .map(|f| f.name.as_str())
                .collect();
            return Err(Diag::new(
                e.span,
                format!(
                    "構造体 `{}` に `{}` というフィールドはありません",
                    ot.name(),
                    field
                ),
            )
            .hint(format!(
                "あるのは {} です",
                if names.is_empty() {
                    "（なし）".to_string()
                } else {
                    names.join("、")
                }
            )));
        };
        let f = &self.ctx.structs[i].fields[j];
        let (offset, fty) = (f.offset, f.ty.clone());
        let dst = self.temp();
        self.emit(Inst::ElemAddr {
            dst,
            base: ov,
            index: Operand::Imm(offset),
            scale: 1,
        });
        let slots = self.ctx.field_slots(i, j);
        Ok((Operand::Temp(dst), fty, slots))
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
        let want_num = want.filter(|w| w.is_numeric()).cloned();
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
            let v = self.rt_value(Rt::Concat, vec![av, bv], e.span)?;
            return Ok((v, Ty::Str));
        }
        if t == Ty::Bool && is_logic(op) {
            return self.logic_bool(op, a, b);
        }
        if t == Ty::Dec {
            let fop = match op {
                BinOp::Add => FOp::Add,
                BinOp::Sub => FOp::Sub,
                BinOp::Mul => FOp::Mul,
                BinOp::Div => FOp::Div,
                BinOp::Mod => FOp::Mod,
                BinOp::Pow => FOp::Pow,
                _ => {
                    return Err(
                        Diag::new(e.span, format!("`{}` は `decnum` には使えません", op.as_str()))
                            .hint("ビットの演算は整数にだけ使えます"),
                    )
                }
            };
            let av = self.expr_as(a, &t)?;
            let bv = self.expr_as(b, &t)?;
            let dst = self.temp();
            self.emit(Inst::FBin {
                dst,
                op: fop,
                a: av,
                b: bv,
                span: e.span,
            });
            return Ok((Operand::Temp(dst), t));
        }
        let Some(it) = t.int_like() else {
            let d = Diag::new(
                e.span,
                format!("`{}` は `{}` には使えません", op.as_str(), t.name()),
            );
            return Err(match t {
                Ty::Bool => d.hint("真偽の演算には `and`・`or`・`not` などを使います"),
                Ty::Str => d.hint("文字列でできるのは `+`（連結）と `==`・`!=` だけです"),
                Ty::Maybe(_) => maybe_hint(d),
                Ty::Struct(..) => d.hint(match op {
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => format!(
                        "構造体の計算は関数で書きます（bignum なら `bignum.{}(a, b)`）",
                        match op {
                            BinOp::Add => "add",
                            BinOp::Sub => "sub",
                            BinOp::Mul => "mul",
                            BinOp::Div => "div",
                            _ => "mod",
                        }
                    ),
                    BinOp::Pow => {
                        "構造体の計算は関数で書きます（bignum なら `bignum.pow(a, n)`）".to_string()
                    }
                    _ => "構造体の計算は関数で書きます".to_string(),
                }),
                t if t.contains_param() => d.hint(PARAM_HINT),
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
            // maybe の変数は、絞り込まれていても、箱を開けずにそのまま比べる。
            let ov = if let Some(var) = self.maybe_local(other) {
                let dst = self.temp();
                self.emit(Inst::Copy {
                    dst,
                    src: Operand::Var(var),
                });
                Operand::Temp(dst)
            } else {
                let (ov, ot) = self.expr(other, None)?;
                if !ot.is_maybe() {
                    return Err(Diag::new(
                        other.span,
                        format!(
                            "`{}` 型の値は null になりません（maybe ではありません）",
                            ot.name()
                        ),
                    )
                    .hint("null と比べられるのは、maybe の付いた型の値だけです"));
                }
                ov
            };
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
        if t.contains_param() {
            return Err(Diag::new(e.span, format!("`{}` 型の値は比べられません", t.name())).hint(PARAM_HINT));
        }
        if let Ty::Maybe(inner) = &t {
            if inner.needs_box() {
                return Err(maybe_hint(Diag::new(
                    e.span,
                    format!("`{}` 型の値は、null でないことを確かめてから比べます", t.name()),
                )));
            }
        }
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
                    let eq = self.rt_value(Rt::StrEq, vec![av, bv], e.span)?;
                    if op == BinOp::Eq {
                        return Ok((eq, Ty::Bool));
                    }
                    self.emit(Inst::BoolNot { dst, a: eq });
                } else if *base == Ty::Dec {
                    self.emit(Inst::FCmp {
                        dst,
                        op: cop,
                        a: av,
                        b: bv,
                    });
                } else {
                    // array・pointer・構造体・関数は、同じものを指しているかを比べる。
                    self.emit(Inst::Cmp {
                        dst,
                        op: cop,
                        a: av,
                        b: bv,
                    });
                }
            }
            _ => {
                if t == Ty::Dec {
                    self.emit(Inst::FCmp {
                        dst,
                        op: cop,
                        a: av,
                        b: bv,
                    });
                } else if t.int_like().is_some() {
                    self.emit(Inst::Cmp {
                        dst,
                        op: cop,
                        a: av,
                        b: bv,
                    });
                } else {
                    let d = Diag::new(
                        e.span,
                        format!(
                            "`{}` で大小を比べられるのは数だけですが、この式は `{}` 型です",
                            op.as_str(),
                            t.name()
                        ),
                    );
                    return Err(match t {
                        Ty::Struct(..) => d.hint(format!(
                            "構造体は関数で比べます（bignum なら `bignum.compare(a, b) {} 0`）",
                            op.as_str()
                        )),
                        Ty::Str => d.hint("文字列の順番は `strings.compare(a, b)` で比べます"),
                        _ => d,
                    });
                }
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
        if self.const_ctx() {
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
        let result = self.new_var(false);
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
        if let ExprKind::Var(name) = &callee.kind {
            if self.lookup(name).is_none() && BUILTINS.contains(&name.as_str()) {
                return self.builtin_call(e, name, args, want);
            }
            if self.lookup(name).is_none() && self.top(name).is_none() && !self.is_namespace(name) {
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
        }
        // `get:32(…)`、`List:32(…)`
        if let ExprKind::Generic(inner, types) = &callee.kind {
            let Some((target, shown)) = self.callee_target(inner) else {
                return Err(Diag::new(
                    callee.span,
                    "型を付けられるのは、ジェネリクスの関数と構造体の名前だけです",
                ));
            };
            return match target {
                Target::Top(TopName::GenericFunc(g)) => {
                    let n = self.ctx.generic_funcs[g].ast.type_params.len();
                    let targs = self.type_args(types, n, &shown)?;
                    self.call_generic(e, g, targs, args)
                }
                Target::Top(TopName::GenericStruct(g)) => {
                    let n = self.ctx.generic_structs[g].def.type_params.len();
                    let targs = self.type_args(types, n, &shown)?;
                    let i = self.ctx.instantiate_struct(g, targs, callee.span)?;
                    self.construct(e, i, args).map(Some)
                }
                _ => Err(Diag::new(
                    callee.span,
                    format!("`{}` はジェネリクスではないので、型は付けられません", shown),
                )),
            };
        }
        match self.callee_target(callee) {
            Some((Target::Stdio, _)) => {
                let ExprKind::Field(obj, f) = &callee.kind else {
                    unreachable!("stdio は ns.name の形")
                };
                let ExprKind::Var(ns) = &obj.kind else {
                    unreachable!("stdio は ns.name の形")
                };
                self.stdio_call(e, ns, f, args)
            }
            Some((Target::Top(TopName::Func(i)), _)) => self.call_func(e, i, args),
            Some((Target::Top(TopName::GenericFunc(g)), shown)) => {
                let targs = self.infer_type_args(e, g, args, &shown, want)?;
                self.call_generic(e, g, targs, args)
            }
            Some((Target::Top(TopName::Struct(i)), _)) => self.construct(e, i, args).map(Some),
            Some((Target::Top(TopName::GenericStruct(g)), shown)) => {
                let n = self.ctx.generic_structs[g].def.type_params.len();
                let example: Vec<&str> = vec!["32"; n];
                Err(Diag::new(
                    callee.span,
                    format!("構造体 `{}` を作るときは、型を書きます", shown),
                )
                .hint(format!("例：{}:{}(…)", shown, example.join(":"))))
            }
            // グローバル変数・ローカル変数・フィールドなどに入った関数（関数ポインタ）
            Some((Target::Top(TopName::Global(_)), _)) | None => self.call_indirect(e, callee, args),
        }
    }

    /// 変数などに入った関数（関数ポインタ）を呼ぶ。
    fn call_indirect(
        &mut self,
        e: &Expr,
        callee: &Expr,
        args: &[Expr],
    ) -> Result<Option<(Operand, Ty)>, Diag> {
        if self.const_ctx() {
            return Err(Diag::new(e.span, "グローバル変数の初期値では、関数を呼べません"));
        }
        let (fv, ft) = self.expr(callee, None)?;
        let (ret, params) = match ft {
            Ty::Func(ret, params) => (ret.map(|t| *t), params),
            Ty::Maybe(inner) if matches!(*inner, Ty::Func(..)) => {
                return Err(maybe_hint(Diag::new(
                    callee.span,
                    format!(
                        "`maybe:{}` 型の関数は、null でないことを確かめてから呼びます",
                        inner.name()
                    ),
                )))
            }
            t => {
                return Err(Diag::new(
                    callee.span,
                    format!("関数ではないので呼べません（この式は `{}` 型です）", t.name()),
                ))
            }
        };
        if args.len() != params.len() {
            return Err(Diag::new(
                e.span,
                format!(
                    "この関数の引数は {} 個ですが、{} 個書かれています",
                    params.len(),
                    args.len()
                ),
            ));
        }
        let mut ops = Vec::new();
        for (arg, ty) in args.iter().zip(&params) {
            ops.push(self.expr_as(arg, ty)?);
        }
        let dst = ret.as_ref().map(|_| self.temp());
        self.emit(Inst::CallIndirect {
            dst,
            target: fv,
            args: ops,
        });
        Ok(ret.map(|t| (Operand::Temp(dst.unwrap()), t)))
    }

    /// ジェネリクスの関数の型引数を、引数の型から決める。
    fn infer_type_args(
        &mut self,
        e: &Expr,
        g: usize,
        args: &[Expr],
        shown: &str,
        want: Option<&Ty>,
    ) -> Result<Vec<Ty>, Diag> {
        let (names, params, ret) = {
            let gf = &self.ctx.generic_funcs[g];
            (gf.ast.type_params.clone(), gf.params.clone(), gf.ret.clone())
        };
        if args.len() != params.len() {
            return Err(Diag::new(
                e.span,
                format!(
                    "関数 `{}` の引数は {} 個ですが、{} 個書かれています",
                    shown,
                    params.len(),
                    args.len()
                ),
            ));
        }
        let mut bind = vec![None; names.len()];
        for (arg, t) in args.iter().zip(&params) {
            if let Some(at) = self.peek(arg) {
                unify(self.ctx, t, &at, &mut bind);
            }
        }
        // 戻り値を入れる先の型からも決める（`h:32 = id(4)`）。
        if let (Some(r), Some(w)) = (&ret, want) {
            unify(self.ctx, r, w, &mut bind);
        }
        // それでも決まらず、引数が整数リテラルなら、整数リテラルの決まりどおり 32 にする。
        for (arg, t) in args.iter().zip(&params) {
            if let (Ty::Param(i, _), None) = (t, self.peek(arg)) {
                if bind[*i as usize].is_none() && is_int_literal(arg) {
                    bind[*i as usize] = Some(I32);
                }
            }
        }
        let mut out = Vec::new();
        for (b, n) in bind.into_iter().zip(&names) {
            match b {
                Some(t) => out.push(t),
                None => {
                    let example: Vec<&str> = vec!["32"; names.len()];
                    return Err(
                        Diag::new(e.span, format!("型引数 `{}` が、引数から決まりません", n)).hint(format!(
                            "`{}:{}(…)` のように型を書きます",
                            shown,
                            example.join(":")
                        )),
                    );
                }
            }
        }
        Ok(out)
    }

    /// ジェネリクスの関数を、型を決めて呼ぶ。
    fn call_generic(
        &mut self,
        e: &Expr,
        g: usize,
        targs: Vec<Ty>,
        args: &[Expr],
    ) -> Result<Option<(Operand, Ty)>, Diag> {
        if self.const_ctx() {
            return Err(Diag::new(e.span, "グローバル変数の初期値では、関数を呼べません"));
        }
        if let Some(bad) = targs.iter().find(|t| **t == Ty::Null) {
            return Err(Diag::new(
                e.span,
                format!("型引数に `{}` は使えません", bad.name()),
            ));
        }
        if self.mode == Mode::Check || targs.iter().any(Ty::contains_param) {
            // 確かめるだけのときは展開しない。引数と戻り値の型だけ求めて、呼び出しの形を確かめる。
            let (name, ast, module) = {
                let gf = &self.ctx.generic_funcs[g];
                (gf.ast.name.clone(), gf.ast, gf.module)
            };
            let tp: TParams = ast.type_params.iter().cloned().zip(targs).collect();
            let (params, ret) = self.ctx.func_types(module, ast, &tp)?;
            if args.len() != params.len() {
                return Err(Diag::new(
                    e.span,
                    format!(
                        "関数 `{}` の引数は {} 個ですが、{} 個書かれています",
                        name,
                        params.len(),
                        args.len()
                    ),
                ));
            }
            for (arg, ty) in args.iter().zip(&params) {
                self.expr_as(arg, ty)?;
            }
            let dst = ret.as_ref().map(|_| self.temp());
            return Ok(ret.map(|t| (Operand::Temp(dst.unwrap()), t)));
        }
        let i = self.ctx.instantiate_func(g, targs, e.span)?;
        self.call_func(e, i, args)
    }

    fn call_func(&mut self, e: &Expr, index: usize, args: &[Expr]) -> Result<Option<(Operand, Ty)>, Diag> {
        if self.const_ctx() {
            return Err(Diag::new(e.span, "グローバル変数の初期値では、関数を呼べません")
                .hint("初期値には、値がコンパイル時に決まる式だけが書けます"));
        }
        let sig = self.ctx.funcs[index].sig.clone();
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

    /// `Point(1, 2)`：構造体を作る。
    fn construct(&mut self, e: &Expr, index: usize, args: &[Expr]) -> Result<(Operand, Ty), Diag> {
        let name = self.ctx.structs[index].name.clone();
        let fields: Vec<(String, Ty, i64)> = self.ctx.structs[index]
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.ty.clone(), f.offset))
            .collect();
        if args.len() != fields.len() {
            let names: Vec<&str> = fields.iter().map(|f| f.0.as_str()).collect();
            return Err(Diag::new(
                e.span,
                format!(
                    "構造体 `{}` のフィールドは {} 個ですが、{} 個書かれています",
                    name,
                    fields.len(),
                    args.len()
                ),
            )
            .hint(format!("フィールドの順に値を渡します（{}）", names.join("、"))));
        }
        let mut values = Vec::new();
        for (arg, (_, ty, _)) in args.iter().zip(&fields) {
            values.push(self.expr_as(arg, ty)?);
        }
        let size = self.ctx.structs[index].size;
        let p = self.rt_value(Rt::Alloc, vec![Operand::Imm(size)], e.span)?;
        for (j, (v, (_, ty, offset))) in values.into_iter().zip(&fields).enumerate() {
            let addr = self.temp();
            self.emit(Inst::ElemAddr {
                dst: addr,
                base: p,
                index: Operand::Imm(*offset),
                scale: 1,
            });
            let slots = self.ctx.field_slots(index, j);
            self.emit_slots(
                Inst::StoreMem {
                    addr: Operand::Temp(addr),
                    src: v,
                    width: ty.width(),
                },
                slots,
            );
        }
        Ok((p, self.ctx.struct_ty(index)))
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
                if self.const_ctx() {
                    // 長さは実行してわかるので、初期化の関数で計算する。
                    self.needs_init = true;
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
                if !matches!(nt, Ty::Int(_)) {
                    return Err(Diag::new(
                        args[0].span,
                        format!("大きさは整数ですが、この式は `{}` 型です", nt.name()),
                    ));
                }
                let v = self.rt_value(Rt::Alloc, vec![n], e.span)?;
                Ok(Some((v, ptr)))
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
            "copy" => {
                self.arg_count(e, name, args, 1)?;
                let (v, t) = self.expr(&args[0], None)?;
                let r = match &t {
                    Ty::Struct(i, _) => {
                        let size = self.ctx.structs[*i].size;
                        self.rt_value(Rt::Copy, vec![v, Operand::Imm(size)], e.span)?
                    }
                    Ty::Array(elem) => {
                        let w = elem.width().bytes();
                        let scan = Operand::Imm(elem.has_pointers() as i64);
                        self.rt_value(Rt::ArrayCopy, vec![v, Operand::Imm(w), scan], e.span)?
                    }
                    Ty::Maybe(_) => {
                        return Err(maybe_hint(Diag::new(
                            args[0].span,
                            format!("`{}` 型の値は、null でないことを確かめてから複製します", t.name()),
                        )))
                    }
                    _ => {
                        return Err(Diag::new(
                            args[0].span,
                            format!(
                                "`copy` は構造体と array にだけ使えますが、この式は `{}` 型です",
                                t.name()
                            ),
                        ))
                    }
                };
                // 複製したものは、もとと同じものを指す（arena の持ち出しの検査に使う）。
                let slots = self.ctx.copy_slots(&t);
                *self.slots.last_mut().unwrap() = slots;
                Ok(Some((r, t)))
            }
            "size" => Err(Diag::new(e.span, "`size` には型を書きます").hint("例：size(:32)")),
            "collect" => {
                self.arg_count(e, name, args, 0)?;
                self.rt(Rt::Collect, vec![], e.span, false)?;
                Ok(None)
            }
            _ => unreachable!("組み込みの関数は上ですべて扱う"),
        }
    }

    /// `stdio.print(…)` など。
    fn stdio_call(
        &mut self,
        e: &Expr,
        ns: &str,
        name: &str,
        args: &[Expr],
    ) -> Result<Option<(Operand, Ty)>, Diag> {
        let full = format!("{}.{}", ns, name);
        let maybe_str = Ty::Maybe(Box::new(Ty::Str));
        match name {
            "print" | "put" | "eprint" | "eput" => {
                if args.is_empty() {
                    return Err(Diag::new(e.span, format!("`{}` には、表示する値を渡します", full))
                        .hint("書式を使うなら `stdio.print(\"{} + {} = {}\", a, b, a + b)` のように書きます"));
                }
                // 値が 2 つ以上なら、最初は書式（`{}` のところに、後ろの値が順に入る）。
                let (pieces, first) = if args.len() == 1 {
                    (vec![Piece::Value(0)], 0)
                } else {
                    (format_pieces(&full, &args[0], args.len() - 1)?, 1)
                };
                // 値は、表示する前に左から順に計算する。
                let mut values = Vec::new();
                for a in &args[first..] {
                    values.push(self.expr(a, None)?);
                }
                let to_stderr = name.starts_with('e');
                let newline = name == "print" || name == "eprint";
                let n = pieces.len();
                for (k, piece) in pieces.into_iter().enumerate() {
                    let (v, desc) = match piece {
                        Piece::Text(text) => (Operand::Str(self.ctx.intern(&text)), Ty::Str.desc()),
                        Piece::Value(i) => (values[i].0, values[i].1.desc()),
                    };
                    let desc = Operand::Str(self.ctx.intern(desc.as_bytes()));
                    if to_stderr {
                        // 標準エラー出力（エラーメッセージなど）。
                        let nl = Operand::Imm((newline && k + 1 == n) as i64);
                        self.rt(Rt::EPut, vec![v, desc, nl], e.span, false)?;
                    } else {
                        self.rt(Rt::Put, vec![v, desc], e.span, false)?;
                    }
                }
                if newline && !to_stderr {
                    self.rt(Rt::Newline, vec![], e.span, false)?;
                }
                Ok(None)
            }
            "read_line" => {
                self.arg_count(e, &full, args, 0)?;
                let v = self.rt_value(Rt::ReadLine, vec![], e.span)?;
                Ok(Some((v, maybe_str)))
            }
            "read_file" => {
                self.arg_count(e, &full, args, 1)?;
                let path = self.expr_as(&args[0], &Ty::Str)?;
                let v = self.rt_value(Rt::ReadFile, vec![path], e.span)?;
                Ok(Some((v, maybe_str)))
            }
            "write_file" => {
                self.arg_count(e, &full, args, 2)?;
                let path = self.expr_as(&args[0], &Ty::Str)?;
                let text = self.expr_as(&args[1], &Ty::Str)?;
                let v = self.rt_value(Rt::WriteFile, vec![path, text], e.span)?;
                Ok(Some((v, Ty::Bool)))
            }
            "last_error" => {
                self.arg_count(e, &full, args, 0)?;
                let v = self.rt_value(Rt::LastError, vec![], e.span)?;
                Ok(Some((v, Ty::Str)))
            }
            _ => Err(Diag::new(e.span, format!("`{}` に `{}` はありません", ns, name))
                .hint("stdio にあるのは print、put、eprint、eput、read_line、read_file、write_file、last_error です")),
        }
    }

    /// `to:型(x)`
    fn convert(&mut self, e: &Expr, ty: &TypeExpr, x: &Expr) -> Result<(Operand, Ty), Diag> {
        let to = self.resolve_type(ty)?;
        if to == Ty::Str {
            let (v, from) = self.expr(x, None)?;
            let func = match &from {
                Ty::Str => return Ok((v, Ty::Str)),
                Ty::Int(_) | Ty::Address => Rt::StrFromInt,
                Ty::Bool => Rt::StrFromBool,
                Ty::Dec => Rt::StrFromDec,
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
            let v = self.rt_value(func, vec![v], e.span)?;
            return Ok((v, Ty::Str));
        }
        // 整数リテラルは、変換先の型として読む（`to:64(3000000000)` を書けるように）。
        let want = if to.int_like().is_some() {
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
            (Ty::Int(_), Ty::Dec) => {
                let dst = self.temp();
                self.emit(Inst::IntToDec { dst, a: v });
                Ok((Operand::Temp(dst), to))
            }
            (Ty::Dec, Ty::Int(t)) => {
                let dst = self.temp();
                self.emit(Inst::DecToInt {
                    dst,
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
            // 文字列から数：失敗することがあるので maybe を返す。
            (Ty::Str, Ty::Int(t)) => {
                let r = self.rt_value(
                    Rt::StrToInt,
                    vec![v, Operand::Imm(ir::bits_of(*t) as i64)],
                    e.span,
                )?;
                Ok((r, Ty::Maybe(Box::new(to))))
            }
            (Ty::Str, Ty::Dec) => {
                let r = self.rt_value(Rt::StrToDec, vec![v], e.span)?;
                Ok((r, Ty::Maybe(Box::new(to))))
            }
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
        // 要素を先に計算してから、配列を作る。
        let mut values = Vec::new();
        for el in elems {
            values.push(self.expr_as(el, &elem)?);
        }
        let arr = self.rt_value(
            Rt::ArrayNew,
            vec![
                Operand::Imm(elems.len() as i64),
                Operand::Imm(width.bytes()),
                Operand::Imm(0),
                Operand::Imm(elem.has_pointers() as i64),
            ],
            e.span,
        )?;
        let slots = self.ctx.elem_slots(&elem);
        for (i, v) in values.into_iter().enumerate() {
            let addr = self.temp();
            self.emit(Inst::ElemAddr {
                dst: addr,
                base: arr,
                index: Operand::Imm(i as i64),
                scale: width.bytes(),
            });
            self.emit_slots(
                Inst::StoreMem {
                    addr: Operand::Temp(addr),
                    src: v,
                    width,
                },
                slots.clone(),
            );
        }
        Ok((arr, Ty::Array(Box::new(elem))))
    }

    /// `array(n)`
    fn array_new(
        &mut self,
        e: &Expr,
        n: &Expr,
        init: Option<&Expr>,
        want: Option<&Ty>,
    ) -> Result<(Operand, Ty), Diag> {
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
        if init.is_none() && matches!(elem, Ty::Struct(..) | Ty::Func(..)) {
            return Err(Diag::new(
                e.span,
                format!(
                    "`array:{}` は `array(n)` では作れません（要素の初期値がないため）",
                    elem.name()
                ),
            )
            .hint(format!(
                "`array(n, 初期値)` のように初期値を渡すか、`array:maybe:{}` にします",
                elem.name()
            )));
        }
        let (nv, nt) = self.expr(n, Some(&I32))?;
        if !matches!(nt, Ty::Int(_)) {
            return Err(Diag::new(
                n.span,
                format!("要素の数は整数ですが、この式は `{}` 型です", nt.name()),
            ));
        }
        // 初期値：数などは 0、string は ""、array は空の配列（null にはしない）、maybe は null。
        let kind = match elem {
            Ty::Str => 1,
            Ty::Array(_) => 2,
            _ => 0,
        };
        let arr = self.rt_value(
            Rt::ArrayNew,
            vec![
                nv,
                Operand::Imm(elem.width().bytes()),
                Operand::Imm(kind),
                Operand::Imm(elem.has_pointers() as i64),
            ],
            e.span,
        )?;
        if let Some(init) = init {
            // すべての要素に、同じ値を入れる。
            let v = self.expr_as(init, &elem)?;
            let slots = self.ctx.elem_slots(&elem);
            self.rt(
                Rt::ArrayFill,
                vec![arr, v, Operand::Imm(elem.width().bytes())],
                e.span,
                false,
            )?;
            *self.slots.last_mut().unwrap() = slots;
        }
        Ok((arr, Ty::Array(Box::new(elem))))
    }
}

/// `stdio.print` の書式の、文字の部分と `{}` の部分。
enum Piece {
    Text(Vec<u8>),
    /// 何番目の値か。
    Value(usize),
}

/// 書式（`"{} + {} = {}"`）を分ける。`{{` と `}}` は `{` と `}` そのもの。
/// `{}` の数が値の数と違えば、コンパイルエラー。
fn format_pieces(full: &str, fmt: &Expr, nvalues: usize) -> Result<Vec<Piece>, Diag> {
    let ExprKind::Str(bytes) = &fmt.kind else {
        return Err(Diag::new(
            fmt.span,
            format!(
                "`{}` に値を 2 つ以上渡すときは、最初に書式の文字列を書きます",
                full
            ),
        )
        .hint("例：stdio.print(\"{} + {} = {}\", a, b, a + b)。`{}` のところに、後ろの値が順に入ります"));
    };
    let mut pieces = Vec::new();
    let mut text = Vec::new();
    let mut count = 0;
    let mut i = 0;
    while i < bytes.len() {
        match (bytes[i], bytes.get(i + 1)) {
            (b'{', Some(b'}')) => {
                if !text.is_empty() {
                    pieces.push(Piece::Text(std::mem::take(&mut text)));
                }
                pieces.push(Piece::Value(count));
                count += 1;
                i += 2;
            }
            (b'{', Some(b'{')) | (b'}', Some(b'}')) => {
                text.push(bytes[i]);
                i += 2;
            }
            (b'{' | b'}', _) => {
                return Err(Diag::new(
                    fmt.span,
                    "書式の中で `{` や `}` を表示するときは、`{{` や `}}` と書きます",
                )
                .hint("`{}` のところには、後ろの値が入ります"))
            }
            (b, _) => {
                text.push(b);
                i += 1;
            }
        }
    }
    if !text.is_empty() {
        pieces.push(Piece::Text(text));
    }
    if count != nvalues {
        return Err(Diag::new(
            fmt.span,
            format!("書式の `{{}}` は {} 個ですが、値は {} 個あります", count, nvalues),
        )
        .hint("`{}` 1 つに、値が 1 つずつ入ります"));
    }
    Ok(pieces)
}

fn is_int_literal(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Int(_) => true,
        ExprKind::Unary(UnOp::Neg, x) => is_int_literal(x),
        ExprKind::Binary(op, a, b) if !is_comparison(*op) => is_int_literal(a) && is_int_literal(b),
        _ => false,
    }
}

/// 整数リテラル。`negative` なら `-n`。型は文脈から決め、決まらなければ `32`。
fn int_literal(n: u64, negative: bool, want: Option<&Ty>, span: Span) -> Result<(Operand, Ty), Diag> {
    let ty = match want {
        Some(t @ (Ty::Int(_) | Ty::Address)) => t.clone(),
        // decnum の場所に書いた整数リテラルは、decnum として読む。
        Some(Ty::Dec) => return dec_literal(n as f64, negative, span),
        Some(Ty::Maybe(inner)) if inner.is_numeric() => {
            return int_literal(n, negative, Some(&**inner), span)
        }
        _ => I32,
    };
    let it = ty.int_like().unwrap();
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

/// 小数リテラル。値はビットのまま持つ。
fn dec_literal(f: f64, negative: bool, span: Span) -> Result<(Operand, Ty), Diag> {
    if !f.is_finite() {
        return Err(Diag::new(span, "小数が大きすぎて、decnum の範囲に入りません"));
    }
    let v = if negative { -f } else { f };
    Ok((Operand::Imm(v.to_bits() as i64), Ty::Dec))
}
