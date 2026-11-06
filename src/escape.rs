//! arena の持ち出しの検査。
//!
//! arena の中で作った値（string・array・構造体など）を、arena を抜けた後も届く場所
//! （arena の外で宣言した変数、グローバル変数、arena の外で作ったものの中）に入れたり、
//! return したりしていないかを、コンパイル時に調べる。呼んだ関数の中での代入も含める。
//!
//! ## 深さ
//!
//! 値ごとに「どこで作られたか」を、次の記号の集まりで表す。
//!
//! - `Zero`：arena の外（GC のヒープ、文字列リテラル、グローバル変数の中身）
//! - `F`：この関数が呼ばれたときの arena（どこで呼ばれたかで決まる）
//! - `Param(i)`：i 番目の引数が作られた場所（`F` と同じか、それより外）
//! - `Local(k)`：この関数の中の、k 段目の arena
//!
//! 外ほど長生きで、`Zero ≤ Param(i) ≤ F < Local(1) < Local(2) < …` の順に先に消える。
//! `Param(i)` どうしや、`F` と `Param(i)`・`Zero` の前後は、呼んだ側でしかわからない。
//! そのときは「`F ≤ Param(0)`」のような条件にして関数のまとめ（`Summary`）に入れ、呼んだ側で調べる。
//! まとめは、すべての関数について変わらなくなるまで繰り返して求める（再帰があるため）。
//!
//! ## 上限と下限
//!
//! 値 v を c の中に入れてよいのは、v が c より長生きのとき（深さ(v) ≤ 深さ(c)）。
//! そこで値ごとに、深さの上限 `hi`（入れる値として使う）と下限 `lo`（入れ先として使う）を持つ。
//!
//! c の中から読んだ値は、入れるときに調べてあるので c より長生き。上限は c の上限でよい。
//! 下限もわかるように、中に別のものを入れられる値（構造体・配列など）を入れるときは、
//! 入れ先と同じ深さかも調べる。同じ深さにならないことがある入れ場所の種類（あるフィールドなど）は
//! 「混ざっている」として、そこから読んだものの下限を `Zero` にする（中に入れられるのは Zero のものだけ）。
//!
//! 数から作ったアドレスやポインタ（`to:pointer:8(to:address(123))` など）は調べない（自己責任）。

use std::collections::{BTreeMap, HashMap, VecDeque};

use crate::ir::{Function, Inst, Operand, Place, Program, Rt};
use crate::source::{Diag, SourceFile, Span};

/// 深さの記号の集まり。記号ごとに 1 ビット。
type Set = u128;

const ZERO: Set = 1;
const F: Set = 2;
/// `Local(k)`（k は 1〜16）は 1 + k ビット目。
const LOCAL0: u32 = 1;
const LOCALS: Set = 0xffff << 2;
/// `Param(i)` は 18 + i ビット目。これより後ろの引数は、下限 Zero・上限 F として扱う。
const PARAM0: u32 = 18;
const MAX_PARAMS: u32 = 128 - PARAM0;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Atom {
    Zero,
    F,
    Local(u32),
    Param(u32),
}

fn bit(a: Atom) -> Set {
    match a {
        Atom::Zero => ZERO,
        Atom::F => F,
        Atom::Local(k) => 1 << (LOCAL0 + k),
        Atom::Param(i) => 1 << (PARAM0 + i),
    }
}

fn atoms(mut s: Set) -> impl Iterator<Item = Atom> {
    std::iter::from_fn(move || {
        if s == 0 {
            return None;
        }
        let b = s.trailing_zeros();
        s &= s - 1;
        Some(match b {
            0 => Atom::Zero,
            1 => Atom::F,
            b if b < PARAM0 => Atom::Local(b - LOCAL0),
            b => Atom::Param(b - PARAM0),
        })
    })
}

enum Le {
    Yes,
    No,
    /// 呼んだ側でしかわからない。
    Cond,
}

/// 深さ(a) ≤ 深さ(b) か（a が b より長生きか）。
fn le(a: Atom, b: Atom) -> Le {
    use Atom::*;
    match (a, b) {
        (Zero, _) => Le::Yes,
        _ if a == b => Le::Yes,
        (Local(x), Local(y)) if x <= y => Le::Yes,
        (Local(_), _) => Le::No,
        (F | Param(_), Local(_)) | (Param(_), F) => Le::Yes,
        // (F か Param, Zero か Param)
        _ => Le::Cond,
    }
}

/// 条件の種類。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Kind {
    /// 入れる値が、入れ先より長生き。破ればエラー。
    Outlive,
    /// 入れ場所の種類 k について、入れる値と入れ先が同じ深さ。破れば、その種類は混ざっている。
    Same(u32),
}

/// 値の深さの下限と上限。数などのアドレスでない値は、どちらも空。
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct Val {
    lo: Set,
    hi: Set,
}

impl Val {
    const NONE: Val = Val { lo: 0, hi: 0 };

    fn exact(s: Set) -> Val {
        Val { lo: s, hi: s }
    }

    fn join(self, o: Val) -> Val {
        Val {
            lo: self.lo | o.lo,
            hi: self.hi | o.hi,
        }
    }
}

/// 関数のまとめ。記号は、その関数から見たもの（`Local` は出てこない）。
#[derive(Clone, Default)]
struct Summary {
    /// 戻り値。
    ret: Val,
    /// 呼んだ側で調べる条件（a ≤ b）と、もとになった代入の場所。
    conds: BTreeMap<(Kind, Atom, Atom), Span>,
}

impl Summary {
    /// `new` を足す。変わったら true。
    fn merge(&mut self, new: Summary) -> bool {
        let ret = self.ret.join(new.ret);
        let mut changed = ret != self.ret;
        self.ret = ret;
        for (k, origin) in new.conds {
            if let std::collections::btree_map::Entry::Vacant(e) = self.conds.entry(k) {
                e.insert(origin);
                changed = true;
            }
        }
        changed
    }
}

/// エラーの理由。
#[derive(Clone, Copy)]
enum Why {
    Var,
    Global,
    Heap,
    Ret,
    /// 呼んだ関数の条件（a ≤ b）が破れる。
    Call {
        func: u32,
        a: Atom,
        b: Atom,
        origin: Span,
    },
}

pub fn check(prog: &Program, srcs: &[SourceFile]) -> Result<(), Diag> {
    let n = prog.funcs.len();
    // 関数ポインタで呼ばれうる関数（アドレスを使っているもの）を、引数の数ごとに。
    let mut taken = vec![false; n];
    for f in &prog.funcs {
        for inst in &f.insts {
            for_each_operand(inst, |op| {
                if let Operand::Func(g) = op {
                    taken[g as usize] = true;
                }
            });
        }
    }
    let mut indirect: HashMap<usize, Vec<u32>> = HashMap::new();
    for (g, f) in prog.funcs.iter().enumerate() {
        if taken[g] {
            indirect.entry(f.nparams as usize).or_default().push(g as u32);
        }
    }

    // 関数のまとめが変わったら、呼んでいる関数を調べ直す。
    let mut callers: Vec<Vec<usize>> = vec![Vec::new(); n];
    // 入れ場所の種類が混ざったら、そこから読んでいる関数を調べ直す。
    let mut readers: Vec<Vec<usize>> = vec![Vec::new(); prog.nslots as usize];
    for (c, f) in prog.funcs.iter().enumerate() {
        for (i, inst) in f.insts.iter().enumerate() {
            match inst {
                Inst::Call { func, .. } => callers[*func as usize].push(c),
                Inst::CallIndirect { args, .. } => {
                    for &g in indirect.get(&args.len()).into_iter().flatten() {
                        callers[g as usize].push(c);
                    }
                }
                Inst::Load { .. } => {
                    for &k in &f.slots[i] {
                        readers[k as usize].push(c);
                    }
                }
                _ => {}
            }
        }
    }
    for v in callers.iter_mut().chain(readers.iter_mut()) {
        v.sort_unstable();
        v.dedup();
    }

    let mut sums = vec![Summary::default(); n];
    let mut mixed = vec![false; prog.nslots as usize];
    let mut errors: Vec<Vec<Diag>> = vec![Vec::new(); n];
    let mut queued = vec![true; n];
    let mut work: VecDeque<usize> = (0..n).collect();
    while let Some(g) = work.pop_front() {
        queued[g] = false;
        let (out, new_mixed, errs) = {
            let mut a = Analyzer::new(prog, &prog.funcs[g], &sums, &mixed, &indirect, srcs);
            a.run();
            (a.out, a.new_mixed, a.errors)
        };
        // 最後に調べたときのエラーが答え（それより後に、使ったまとめは変わっていない）。
        errors[g] = errs.into_values().collect();
        let mut again: Vec<usize> = Vec::new();
        if sums[g].merge(out) {
            again.extend(&callers[g]);
        }
        for k in new_mixed {
            if !mixed[k as usize] {
                mixed[k as usize] = true;
                again.extend(&readers[k as usize]);
            }
        }
        for c in again {
            if !queued[c] {
                queued[c] = true;
                work.push_back(c);
            }
        }
    }
    // ソースでいちばん前のエラーを出す。
    match errors
        .into_iter()
        .flatten()
        .min_by_key(|d| (d.span.file, d.span.lo))
    {
        Some(d) => Err(d),
        None => Ok(()),
    }
}

/// 1 つの関数を調べる。
struct Analyzer<'a> {
    prog: &'a Program,
    f: &'a Function,
    sums: &'a [Summary],
    mixed: &'a [bool],
    indirect: &'a HashMap<usize, Vec<u32>>,
    srcs: &'a [SourceFile],
    /// 命令ごとの、arena の深さ。
    depth: Vec<u32>,
    /// 一時変数の値（1 回しか代入しないので、場所ごとに分けなくてよい）。
    temps: Vec<Val>,
    /// 繰り返しの中で、何かが変わったか。
    changed: bool,
    out: Summary,
    new_mixed: Vec<u32>,
    /// 命令の番号ごとのエラー。
    errors: BTreeMap<usize, Diag>,
}

impl<'a> Analyzer<'a> {
    fn new(
        prog: &'a Program,
        f: &'a Function,
        sums: &'a [Summary],
        mixed: &'a [bool],
        indirect: &'a HashMap<usize, Vec<u32>>,
        srcs: &'a [SourceFile],
    ) -> Analyzer<'a> {
        let mut depth = Vec::with_capacity(f.insts.len());
        let mut d = 0;
        for inst in &f.insts {
            depth.push(d);
            match inst {
                Inst::ArenaEnter => d += 1,
                Inst::ArenaExit => d -= 1,
                _ => {}
            }
        }
        Analyzer {
            prog,
            f,
            sums,
            mixed,
            indirect,
            srcs,
            depth,
            temps: vec![Val::NONE; f.ntemps as usize],
            changed: false,
            out: Summary::default(),
            new_mixed: Vec::new(),
            errors: BTreeMap::new(),
        }
    }

    /// 命令 i の場所で新しく作ったものの深さ。
    fn here(&self, i: usize) -> Set {
        match self.depth[i] {
            0 => F,
            k => bit(Atom::Local(k)),
        }
    }

    fn run(&mut self) {
        let f = self.f;
        let n = f.insts.len();
        let mut label_at = HashMap::new();
        for (i, inst) in f.insts.iter().enumerate() {
            if let Inst::Label(l) = inst {
                label_at.insert(*l, i);
            }
        }
        // 引数は Param(i)。ほかの変数は、まだ何も入っていない。
        let mut init = vec![Val::NONE; f.nvars as usize];
        for i in 0..f.nparams {
            if f.ptr_vars[i as usize] {
                init[i as usize] = if i < MAX_PARAMS {
                    Val::exact(bit(Atom::Param(i)))
                } else {
                    Val { lo: ZERO, hi: F }
                };
            }
        }
        // ラベルごとの、そこへ来るときの変数の値。
        let mut at_label: Vec<Option<Vec<Val>>> = vec![None; n];
        loop {
            self.changed = false;
            let mut cur = Some(init.clone());
            for i in 0..n {
                if let Inst::Label(_) = f.insts[i] {
                    if let Some(c) = cur.take() {
                        self.changed |= join_into(&mut at_label[i], &c);
                    }
                    cur = at_label[i].clone();
                }
                // たどり着けない命令は調べない。
                let Some(st) = cur.as_mut() else { continue };
                self.step(i, st);
                match &f.insts[i] {
                    Inst::Jump(l) => {
                        self.changed |= join_into(&mut at_label[label_at[l]], st);
                        cur = None;
                    }
                    Inst::Branch { then, els, .. } => {
                        self.changed |= join_into(&mut at_label[label_at[then]], st);
                        self.changed |= join_into(&mut at_label[label_at[els]], st);
                        cur = None;
                    }
                    Inst::Ret(_) => cur = None,
                    _ => {}
                }
            }
            if !self.changed {
                break;
            }
        }
    }

    fn val(&self, st: &[Val], op: Operand) -> Val {
        match op {
            Operand::Temp(t) => self.temps[t as usize],
            Operand::Var(v) => st[v as usize],
            Operand::Global(_) | Operand::Str(_) => Val::exact(ZERO),
            Operand::Imm(_) | Operand::Func(_) => Val::NONE,
        }
    }

    fn set(&mut self, dst: u32, v: Val) {
        let t = &mut self.temps[dst as usize];
        let j = t.join(v);
        if j != *t {
            *t = j;
            self.changed = true;
        }
    }

    fn pure(&self, k: u32) -> bool {
        !self.mixed[k as usize]
    }

    /// 命令 i を実行したあとの変数の値にする。代入を調べる。
    fn step(&mut self, i: usize, st: &mut [Val]) {
        let f = self.f;
        match &f.insts[i] {
            Inst::Bin { dst, a, b, .. } => {
                let v = self.val(st, *a).join(self.val(st, *b));
                self.set(*dst, v);
            }
            // アドレスの計算（`to:address(p) + 16` など）では、もとのものの深さを引き継ぐ。
            Inst::Neg { dst, a, .. }
            | Inst::Not { dst, a, .. }
            | Inst::Convert { dst, a, .. }
            | Inst::Copy { dst, src: a } => {
                let v = self.val(st, *a);
                self.set(*dst, v);
            }
            Inst::ElemAddr { dst, base, .. } => {
                let v = self.val(st, *base);
                self.set(*dst, v);
            }
            Inst::Store { place, src } => {
                let s = self.val(st, *src);
                match place {
                    Place::Var(v) => {
                        let bound = match f.var_depth[*v as usize] {
                            0 => F,
                            k => bit(Atom::Local(k)),
                        };
                        self.check(i, Kind::Outlive, s.hi, bound, Why::Var);
                        st[*v as usize] = s;
                    }
                    Place::Global(_) => self.check(i, Kind::Outlive, s.hi, ZERO, Why::Global),
                }
            }
            Inst::Load { dst, addr, .. } => {
                if f.ptr_temps[*dst as usize] {
                    let a = self.val(st, *addr);
                    let lo = match f.slots[i].as_slice() {
                        [k] if self.pure(*k) => a.lo,
                        _ => ZERO,
                    };
                    self.set(*dst, Val { lo, hi: a.hi });
                }
            }
            Inst::StoreMem { addr, src, .. } => {
                let (a, s) = (self.val(st, *addr), self.val(st, *src));
                self.check(i, Kind::Outlive, s.hi, a.lo, Why::Heap);
                if let [k] = f.slots[i].as_slice() {
                    if self.pure(*k) {
                        self.check(i, Kind::Same(*k), a.hi, s.lo, Why::Heap);
                    }
                }
            }
            Inst::Call { dst, func, args } => self.call(i, st, *dst, &[*func], args),
            Inst::CallIndirect { dst, args, .. } => {
                let indirect = self.indirect;
                let callees = indirect.get(&args.len()).map_or(&[][..], |v| v.as_slice());
                self.call(i, st, *dst, callees, args);
            }
            Inst::CallRt { dst, func, args, .. } => {
                let here = self.here(i);
                match func {
                    Rt::Alloc
                    | Rt::ArrayNew
                    | Rt::Concat
                    | Rt::StrFromInt
                    | Rt::StrFromBool
                    | Rt::StrFromDec
                    | Rt::ReadLine
                    | Rt::ReadFile
                    | Rt::LastError => {
                        if let Some(d) = dst {
                            self.set(*d, Val::exact(here));
                        }
                    }
                    // 複製したものは、もとの中身と同じものを指す。
                    Rt::Copy | Rt::ArrayCopy => {
                        if let Some(d) = dst {
                            self.set(*d, Val::exact(here));
                        }
                        let orig = self.val(st, args[0]);
                        for &k in &f.slots[i] {
                            if self.pure(k) {
                                self.check(i, Kind::Same(k), here, orig.lo, Why::Heap);
                            }
                        }
                    }
                    // maybe:32 などの箱は、arena の中でも GC 側に作る。
                    Rt::Box
                    | Rt::StrToInt
                    | Rt::StrToDec
                    | Rt::StrEq
                    | Rt::Pow(_)
                    | Rt::Put
                    | Rt::Newline
                    | Rt::WriteFile
                    | Rt::Collect => {}
                }
            }
            Inst::Ret(Some(v)) => {
                let v = self.val(st, *v);
                if v.hi & LOCALS != 0 {
                    self.error(i, Why::Ret);
                }
                let v = Val {
                    lo: v.lo & !LOCALS,
                    hi: v.hi & !LOCALS,
                };
                self.out.ret = self.out.ret.join(v);
            }
            Inst::BoolNot { .. }
            | Inst::Cmp { .. }
            | Inst::FBin { .. }
            | Inst::FNeg { .. }
            | Inst::FCmp { .. }
            | Inst::IntToDec { .. }
            | Inst::DecToInt { .. }
            | Inst::CheckIndex { .. }
            | Inst::Length { .. }
            | Inst::Label(_)
            | Inst::Jump(_)
            | Inst::Branch { .. }
            | Inst::Ret(None)
            | Inst::ArenaEnter
            | Inst::ArenaExit
            | Inst::ArenaPop { .. } => {}
        }
    }

    /// 関数を呼ぶ。呼ばれうる関数のまとめを、ここでの値に当てはめる。
    fn call(&mut self, i: usize, st: &[Val], dst: Option<u32>, callees: &[u32], args: &[Operand]) {
        let here = self.here(i);
        let argv: Vec<Val> = args.iter().map(|a| self.val(st, *a)).collect();
        let map = |s: Set, lo: bool| -> Set {
            let mut r = 0;
            for a in atoms(s) {
                r |= match a {
                    Atom::Zero => ZERO,
                    Atom::F => here,
                    Atom::Param(p) => argv.get(p as usize).map_or(0, |v| if lo { v.lo } else { v.hi }),
                    Atom::Local(_) => unreachable!("まとめに Local は入れない"),
                };
            }
            r
        };
        let sums = self.sums;
        let mut ret = Val::NONE;
        for &g in callees {
            let sum = &sums[g as usize];
            ret = ret.join(Val {
                lo: map(sum.ret.lo, true),
                hi: map(sum.ret.hi, false),
            });
            for (&(kind, a, b), &origin) in &sum.conds {
                if let Kind::Same(k) = kind {
                    if !self.pure(k) {
                        continue;
                    }
                }
                let why = Why::Call {
                    func: g,
                    a,
                    b,
                    origin,
                };
                self.check(i, kind, map(bit(a), false), map(bit(b), true), why);
            }
        }
        if let Some(d) = dst {
            self.set(d, ret);
        }
    }

    /// 深さ(src の値) ≤ 深さ(dst の値) を調べる。わからなければ、呼んだ側で調べる条件にする。
    fn check(&mut self, i: usize, kind: Kind, src: Set, dst: Set, why: Why) {
        for a in atoms(src) {
            for b in atoms(dst) {
                match le(a, b) {
                    Le::Yes => {}
                    Le::No => match kind {
                        Kind::Outlive => self.error(i, why),
                        Kind::Same(k) => self.new_mixed.push(k),
                    },
                    Le::Cond => {
                        let origin = match why {
                            Why::Call { origin, .. } => origin,
                            _ => self.f.spans[i],
                        };
                        self.out.conds.entry((kind, a, b)).or_insert(origin);
                    }
                }
            }
        }
    }

    fn error(&mut self, i: usize, why: Why) {
        if self.errors.contains_key(&i) {
            return;
        }
        let span = self.f.spans[i];
        const GONE: &str = "arena の中で作ったものは、arena を抜けると消えます";
        let d = match why {
            Why::Var => Diag::new(
                span,
                "arena の中で作った値を、arena の外で宣言した変数に入れています",
            )
            .hint(format!(
                "{}。変数を arena の中で宣言するか、値を arena の外で作ります",
                GONE
            )),
            Why::Global => Diag::new(span, "arena の中で作った値を、グローバル変数に入れています").hint(GONE),
            Why::Heap => Diag::new(
                span,
                "arena の中で作った値を、arena の外で作ったもの（構造体・配列など）の中に入れています",
            )
            .hint(GONE),
            Why::Ret => Diag::new(span, "arena の中で作った値を return しています").hint(GONE),
            Why::Call { func, a, b, origin } => {
                let name = &self.prog.funcs[func as usize].name;
                let what = match a {
                    Atom::Param(p) => format!(
                        "関数 `{}` に渡した {} 番目の引数（arena の中で作った値）",
                        name,
                        p + 1
                    ),
                    _ => format!(
                        "関数 `{}` が中で作る値（ここで呼ぶと arena の中にできます）",
                        name
                    ),
                };
                let to = match b {
                    Atom::Param(q) => {
                        format!("{} 番目の引数（それより外で作ったもの）の中に入れられます", q + 1)
                    }
                    _ => "arena の外から届く場所（グローバル変数など）に残されます".to_string(),
                };
                Diag::new(span, format!("{}が、{}", what, to)).hint(format!(
                    "{}（入れているのは {}）",
                    GONE,
                    self.place(origin)
                ))
            }
        };
        self.errors.insert(i, d);
    }

    /// 「ファイル名:行」
    fn place(&self, span: Span) -> String {
        match self.srcs.get(span.file as usize) {
            Some(src) => format!("{}:{}", src.name, src.line_of(span.lo)),
            None => "?".to_string(),
        }
    }
}

/// `to` に `from` を合わせる。変わったら true。
fn join_into(to: &mut Option<Vec<Val>>, from: &[Val]) -> bool {
    match to {
        None => {
            *to = Some(from.to_vec());
            true
        }
        Some(t) => {
            let mut changed = false;
            for (x, y) in t.iter_mut().zip(from) {
                let j = x.join(*y);
                if j != *x {
                    *x = j;
                    changed = true;
                }
            }
            changed
        }
    }
}

fn for_each_operand(inst: &Inst, mut f: impl FnMut(Operand)) {
    match inst {
        Inst::Bin { a, b, .. }
        | Inst::Cmp { a, b, .. }
        | Inst::FBin { a, b, .. }
        | Inst::FCmp { a, b, .. } => {
            f(*a);
            f(*b);
        }
        Inst::Neg { a, .. }
        | Inst::Not { a, .. }
        | Inst::BoolNot { a, .. }
        | Inst::Convert { a, .. }
        | Inst::FNeg { a, .. }
        | Inst::IntToDec { a, .. }
        | Inst::DecToInt { a, .. }
        | Inst::Length { a, .. }
        | Inst::Copy { src: a, .. }
        | Inst::Store { src: a, .. }
        | Inst::Load { addr: a, .. }
        | Inst::Branch { cond: a, .. }
        | Inst::Ret(Some(a)) => f(*a),
        Inst::StoreMem { addr, src, .. } => {
            f(*addr);
            f(*src);
        }
        Inst::ElemAddr { base, index, .. } => {
            f(*base);
            f(*index);
        }
        Inst::CheckIndex { array, index, .. } => {
            f(*array);
            f(*index);
        }
        Inst::Call { args, .. } | Inst::CallRt { args, .. } => args.iter().for_each(|a| f(*a)),
        Inst::CallIndirect { target, args, .. } => {
            f(*target);
            args.iter().for_each(|a| f(*a));
        }
        Inst::Label(_)
        | Inst::Jump(_)
        | Inst::Ret(None)
        | Inst::ArenaEnter
        | Inst::ArenaExit
        | Inst::ArenaPop { .. } => {}
    }
}
