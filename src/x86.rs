//! x86-64（Linux）向けのアセンブリ（GNU as、AT&T 記法）を出す。
//!
//! C ライブラリは使わない。表示や文字列の連結などは、ランタイム（runtime/orz_runtime.c）が受け持つ。
//! 起動の流れ：`_start` → `orz_rt_start`（C）→ `orz_entry`（ここで出す）→ `main`。
//!
//! 一時変数と変数はすべてスタックに置く（速さより、まず正しく動くことを優先）。
//! 値はすべて 8 バイトで持つ。`32` は符号を広げ、`8` と `bool` は 0 を詰めた形にそろえておく。
//! `decnum` はビットのまま持ち、計算するときだけ %xmm0・%xmm1 に移す。
//!
//! 関数の呼び出し方は C（System V ABI）と同じ：引数は %rdi, %rsi, %rdx, %rcx, %r8, %r9、
//! 7 個目からはスタック、戻り値は %rax。

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use crate::ast::IntTy;
use crate::ir::{self, CmpOp, ConstVal, FOp, Inst, IrBinOp, Operand, Place, Rt, Trap, Width};
use crate::source::{SourceFile, Span};

const ARG_REGS: [&str; 6] = ["%rdi", "%rsi", "%rdx", "%rcx", "%r8", "%r9"];

/// `gc_disabled` なら GC を動かさない（終了時にまとめて解放する）。
pub fn emit(prog: &ir::Program, srcs: &[SourceFile], gc_disabled: bool) -> String {
    let mut g = Gen {
        out: String::new(),
        gc_disabled,
        srcs,
        prog,
        func: 0,
        label: 0,
        sites: BTreeMap::new(),
        messages: HashMap::new(),
        traps: Vec::new(),
        temp_slots: Vec::new(),
        nslots: 0,
    };
    g.header();
    for (i, f) in prog.funcs.iter().enumerate() {
        g.func = i;
        g.function(f);
    }
    g.data();
    g.out
}

/// 実行時エラーへの飛び先で、何を表示するか。
enum TrapKind {
    /// 理由の文だけ。
    Message(String),
    /// 配列の範囲外。%rcx = 添字、%rdx = 長さ。
    Index,
    /// 整数の変換で範囲外。%rax = 値。
    Convert(IntTy),
    /// decnum から整数への変換で範囲外。%xmm0 = 値。
    ConvertDec(IntTy),
}

/// 実行時エラーの場所（ファイル番号, 行）。
type Site = (u32, usize);

struct Gen<'a> {
    out: String,
    gc_disabled: bool,
    srcs: &'a [SourceFile],
    prog: &'a ir::Program,
    /// 今出している関数の番号。
    func: usize,
    label: u32,
    /// 実行時エラーで表示する「ファイル名:行」→ ラベル番号。
    sites: BTreeMap<Site, u32>,
    /// 実行時エラーの理由の文 → ラベル番号。
    messages: HashMap<String, u32>,
    /// 関数の後ろにまとめて置く、実行時エラーへの飛び先。（ラベル, 場所, 種類）
    traps: Vec<(u32, Site, TrapKind)>,
    /// 今の関数の、一時変数の置き場所（番号）と、置き場所の数。
    temp_slots: Vec<u32>,
    nslots: u32,
}

macro_rules! asm {
    ($g:expr, $($arg:tt)*) => {{
        let _ = writeln!($g.out, "\t{}", format_args!($($arg)*));
    }};
}

impl<'a> Gen<'a> {
    fn new_label(&mut self) -> u32 {
        self.label += 1;
        self.label
    }

    fn header(&mut self) {
        let main = &self.prog.funcs[self.prog.main as usize].symbol;
        let _ = writeln!(self.out, "# orz が出力したアセンブリ（{}）", self.srcs[0].name);
        asm!(self, ".text");
        asm!(self, ".globl _start");
        let _ = writeln!(self.out, "_start:");
        asm!(self, "xorl %ebp, %ebp");
        asm!(self, "movq %rsp, %rdi");
        asm!(self, "andq $-16, %rsp");
        asm!(self, "call orz_rt_start");
        asm!(self, "hlt");
        // ランタイムから呼ばれる。main を呼んで、終了コードを %rax で返す。
        asm!(self, "\n\t.globl orz_entry");
        let _ = writeln!(self.out, "orz_entry:");
        asm!(self, "pushq %rbp");
        asm!(self, "movq %rsp, %rbp");
        if self.prog.main_takes_args {
            asm!(self, "call orz_rt_args");
            asm!(self, "movq %rax, %rdi");
        }
        // グローバル変数のうち、配列や構造体の初期値を入れる。
        for &i in &self.prog.init_funcs {
            asm!(self, "call {}", self.prog.funcs[i as usize].symbol);
        }
        asm!(self, "call {}", main);
        if !self.prog.main_returns {
            asm!(self, "xorl %eax, %eax");
        }
        asm!(self, "popq %rbp");
        asm!(self, "ret");
    }

    fn slot(n: u32) -> String {
        format!("-{}(%rbp)", (n + 1) * 8)
    }

    fn temp_slot(&self, t: u32) -> String {
        Self::slot(self.temp_slots[t as usize])
    }

    fn var_slot(&self, v: u32) -> String {
        Self::slot(self.nslots + v)
    }

    fn global(&self, g: u32) -> String {
        format!("{}(%rip)", self.prog.globals[g as usize].symbol)
    }

    fn place(&self, p: Place) -> String {
        match p {
            Place::Var(v) => self.var_slot(v),
            Place::Global(g) => self.global(g),
        }
    }

    /// 値を 64bit のレジスタに入れる。
    fn load(&mut self, op: Operand, reg: &str) {
        match op {
            Operand::Imm(v) if i32::try_from(v).is_ok() => asm!(self, "movq ${}, {}", v, reg),
            Operand::Imm(v) => asm!(self, "movabsq ${}, {}", v, reg),
            Operand::Temp(t) => {
                let s = self.temp_slot(t);
                asm!(self, "movq {}, {}", s, reg)
            }
            Operand::Var(v) => {
                let s = self.var_slot(v);
                asm!(self, "movq {}, {}", s, reg)
            }
            Operand::Global(g) => {
                let s = self.global(g);
                asm!(self, "movq {}, {}", s, reg)
            }
            Operand::Str(i) => asm!(self, "leaq .Lstr{}(%rip), {}", i, reg),
            Operand::Func(i) => {
                let s = &self.prog.funcs[i as usize].symbol;
                asm!(self, "leaq {}(%rip), {}", s, reg)
            }
        }
    }

    fn store_temp(&mut self, dst: u32) {
        let s = self.temp_slot(dst);
        asm!(self, "movq %rax, {}", s);
    }

    fn block_label(&self, l: u32) -> String {
        format!(".Lf{}_{}", self.func, l)
    }

    fn site_of(&self, span: Span) -> Site {
        (span.file, self.srcs[span.file as usize].line_of(span.lo))
    }

    fn site(&mut self, site: Site) -> u32 {
        let n = self.sites.len() as u32;
        *self.sites.entry(site).or_insert(n)
    }

    fn message(&mut self, text: String) -> String {
        let n = self.messages.len() as u32;
        format!(".Lmsg{}", *self.messages.entry(text).or_insert(n))
    }

    /// 実行時エラーへ飛ぶ命令を出す。
    fn trap_jump(&mut self, cond: &str, site: Site, kind: TrapKind) {
        let l = self.new_label();
        self.traps.push((l, site, kind));
        asm!(self, "{} .Ltrap{}", cond, l);
    }

    fn trap(&mut self, cond: &str, site: Site, trap: Trap, ty: IntTy) {
        self.trap_jump(cond, site, TrapKind::Message(trap.message(ty)));
    }

    fn function(&mut self, f: &ir::Function) {
        (self.temp_slots, self.nslots) = assign_slots(f);
        let frame = ((self.nslots + f.nvars) * 8).div_ceil(16) * 16;
        let _ = writeln!(self.out, "\n# function {}\n{}:", f.name, f.symbol);
        asm!(self, "pushq %rbp");
        asm!(self, "movq %rsp, %rbp");
        if frame > 0 {
            asm!(self, "subq ${}, %rsp", frame);
        }
        // 引数を変数の場所に写す。
        for i in 0..f.nparams {
            let slot = self.var_slot(i);
            if let Some(reg) = ARG_REGS.get(i as usize) {
                asm!(self, "movq {}, {}", reg, slot);
            } else {
                // 7 個目からはスタック。16(%rbp) が 7 個目。
                asm!(self, "movq {}(%rbp), %rax", 16 + 8 * (i - 6));
                asm!(self, "movq %rax, {}", slot);
            }
        }
        for inst in &f.insts {
            self.inst(inst);
        }
        if f.returns_value {
            // 値を返す関数は必ず return する（意味解析で確認済み）。
            asm!(self, "ud2");
        }
        // 実行時エラーへの飛び先。ここでは %rsp が 16 の倍数なので、そのまま C の関数を呼べる。
        for (l, site, kind) in std::mem::take(&mut self.traps) {
            let site = self.site(site);
            let _ = writeln!(self.out, ".Ltrap{}:", l);
            match kind {
                TrapKind::Message(text) => {
                    let m = self.message(text);
                    asm!(self, "leaq .Lsite{}(%rip), %rdi", site);
                    asm!(self, "leaq {}(%rip), %rsi", m);
                    asm!(self, "call orz_rt_panic");
                }
                TrapKind::Index => {
                    asm!(self, "leaq .Lsite{}(%rip), %rdi", site);
                    asm!(self, "movq %rcx, %rsi");
                    asm!(self, "call orz_rt_panic_index");
                }
                TrapKind::Convert(to) => {
                    let m = self.message(ir::convert_message(to));
                    asm!(self, "movq %rax, %rsi");
                    asm!(self, "leaq .Lsite{}(%rip), %rdi", site);
                    asm!(self, "leaq {}(%rip), %rdx", m);
                    asm!(self, "call orz_rt_panic_convert");
                }
                TrapKind::ConvertDec(to) => {
                    let m = self.message(ir::convert_message(to));
                    asm!(self, "movq %xmm0, %rsi");
                    asm!(self, "leaq .Lsite{}(%rip), %rdi", site);
                    asm!(self, "leaq {}(%rip), %rdx", m);
                    asm!(self, "call orz_rt_panic_convert_dec");
                }
            }
        }
    }

    fn inst(&mut self, inst: &Inst) {
        match inst {
            Inst::Copy { dst, src } => {
                self.load(*src, "%rax");
                self.store_temp(*dst);
            }
            Inst::Store { place, src } => {
                self.load(*src, "%rax");
                let p = self.place(*place);
                asm!(self, "movq %rax, {}", p);
            }
            Inst::Load { dst, addr, width } => {
                self.load(*addr, "%rax");
                match width {
                    Width::W1 => asm!(self, "movzbl (%rax), %eax"),
                    Width::W4 => asm!(self, "movslq (%rax), %rax"),
                    Width::W8 => asm!(self, "movq (%rax), %rax"),
                }
                self.store_temp(*dst);
            }
            Inst::StoreMem { addr, src, width } => {
                self.load(*addr, "%rax");
                self.load(*src, "%rcx");
                match width {
                    Width::W1 => asm!(self, "movb %cl, (%rax)"),
                    Width::W4 => asm!(self, "movl %ecx, (%rax)"),
                    Width::W8 => asm!(self, "movq %rcx, (%rax)"),
                }
            }
            Inst::ElemAddr {
                dst,
                base,
                index,
                scale,
            } => {
                self.load(*base, "%rax");
                match (index, scale) {
                    (Operand::Imm(i), _) if i32::try_from(*i * *scale).is_ok() => {
                        if *i != 0 {
                            asm!(self, "addq ${}, %rax", *i * *scale);
                        }
                    }
                    _ => {
                        self.load(*index, "%rcx");
                        asm!(self, "leaq (%rax,%rcx,{}), %rax", scale);
                    }
                }
                self.store_temp(*dst);
            }
            Inst::CheckIndex { array, index, span } => {
                let site = self.site_of(*span);
                self.load(*array, "%rax");
                asm!(self, "movq -8(%rax), %rdx");
                self.load(*index, "%rcx");
                // 符号なしで比べるので、負の添字もここで弾ける。
                asm!(self, "cmpq %rdx, %rcx");
                self.trap_jump("jae", site, TrapKind::Index);
            }
            Inst::Length { dst, a } => {
                self.load(*a, "%rax");
                asm!(self, "movq -8(%rax), %rax");
                self.store_temp(*dst);
            }
            Inst::Label(l) => {
                let _ = writeln!(self.out, "{}:", self.block_label(*l));
            }
            Inst::Jump(l) => {
                let l = self.block_label(*l);
                asm!(self, "jmp {}", l);
            }
            Inst::Branch { cond, then, els } => {
                self.load(*cond, "%rax");
                asm!(self, "testq %rax, %rax");
                let (t, e) = (self.block_label(*then), self.block_label(*els));
                asm!(self, "jnz {}", t);
                asm!(self, "jmp {}", e);
            }
            Inst::Call { dst, func, args } => {
                let name = self.prog.funcs[*func as usize].symbol.clone();
                self.call(&name, args, &[]);
                if let Some(d) = dst {
                    self.store_temp(*d);
                }
            }
            Inst::CallRt {
                dst,
                func,
                args,
                span,
            } => {
                self.call_rt(*func, args, *span);
                if let Some(d) = dst {
                    self.store_temp(*d);
                }
            }
            Inst::CallIndirect { dst, target, args } => {
                self.call_with_target(None, Some(*target), args, &[]);
                if let Some(d) = dst {
                    self.store_temp(*d);
                }
            }
            Inst::ArenaEnter => asm!(self, "call orz_rt_arena_push"),
            Inst::ArenaExit => asm!(self, "call orz_rt_arena_pop"),
            Inst::ArenaPop { n } => {
                for _ in 0..*n {
                    asm!(self, "call orz_rt_arena_pop");
                }
            }
            Inst::Ret(v) => {
                if let Some(v) = v {
                    self.load(*v, "%rax");
                }
                asm!(self, "leave");
                asm!(self, "ret");
            }
            Inst::Cmp { dst, op, a, b } => {
                self.load(*a, "%rax");
                self.load(*b, "%rcx");
                asm!(self, "cmpq %rcx, %rax");
                let set = match op {
                    CmpOp::Lt => "setl",
                    CmpOp::Gt => "setg",
                    CmpOp::Le => "setle",
                    CmpOp::Ge => "setge",
                    CmpOp::Eq => "sete",
                    CmpOp::Ne => "setne",
                };
                asm!(self, "{} %al", set);
                asm!(self, "movzbl %al, %eax");
                self.store_temp(*dst);
            }
            Inst::BoolNot { dst, a } => {
                self.load(*a, "%rax");
                asm!(self, "xorl $1, %eax");
                self.store_temp(*dst);
            }
            Inst::Not { dst, ty, a } => {
                self.load(*a, "%rax");
                match ty {
                    IntTy::U8 => asm!(self, "xorl $255, %eax"),
                    IntTy::I32 => {
                        asm!(self, "notl %eax");
                        asm!(self, "movslq %eax, %rax");
                    }
                    IntTy::I64 => asm!(self, "notq %rax"),
                }
                self.store_temp(*dst);
            }
            Inst::Neg { dst, ty, a, span } => {
                let site = self.site_of(*span);
                self.load(*a, "%rax");
                match ty {
                    IntTy::U8 => {
                        // 符号なしなので、0 以外の符号を変えると範囲の外になる。
                        asm!(self, "testl %eax, %eax");
                        self.trap("jnz", site, Trap::Overflow, *ty);
                    }
                    IntTy::I32 => {
                        asm!(self, "negl %eax");
                        self.trap("jo", site, Trap::Overflow, *ty);
                        asm!(self, "movslq %eax, %rax");
                    }
                    IntTy::I64 => {
                        asm!(self, "negq %rax");
                        self.trap("jo", site, Trap::Overflow, *ty);
                    }
                }
                self.store_temp(*dst);
            }
            Inst::Convert {
                dst,
                from,
                to,
                a,
                span,
            } => {
                let site = self.site_of(*span);
                self.load(*a, "%rax");
                match (from, to) {
                    (IntTy::I64, IntTy::I32) => {
                        asm!(self, "movslq %eax, %rcx");
                        asm!(self, "cmpq %rax, %rcx");
                        self.trap_jump("jne", site, TrapKind::Convert(*to));
                    }
                    (IntTy::I32 | IntTy::I64, IntTy::U8) => {
                        // 符号なしで比べるので、負の値もここで弾ける。
                        asm!(self, "cmpq $255, %rax");
                        self.trap_jump("ja", site, TrapKind::Convert(*to));
                    }
                    // 広げる向きは、そろえた形のままでよい。
                    _ => {}
                }
                self.store_temp(*dst);
            }
            Inst::Bin {
                dst,
                op,
                ty,
                a,
                b,
                span,
            } => {
                let site = self.site_of(*span);
                if *op == IrBinOp::Pow {
                    self.call_rt(Rt::Pow(*ty), &[*a, *b], *span);
                } else {
                    self.load(*a, "%rax");
                    self.load(*b, "%rcx");
                    match ty {
                        IntTy::I32 => self.bin32(*op, *b, site),
                        IntTy::I64 => self.bin64(*op, *b, site),
                        IntTy::U8 => self.bin8(*op, site),
                    }
                }
                self.store_temp(*dst);
            }
            Inst::FBin { dst, op, a, b, span } => {
                let site = self.site_of(*span);
                self.fbin(*op, *a, *b, site);
                self.store_temp(*dst);
            }
            Inst::FNeg { dst, a } => {
                self.load(*a, "%rax");
                asm!(self, "btcq $63, %rax");
                self.store_temp(*dst);
            }
            Inst::FCmp { dst, op, a, b } => {
                self.load(*a, "%rax");
                self.load(*b, "%rcx");
                asm!(self, "movq %rax, %xmm0");
                asm!(self, "movq %rcx, %xmm1");
                // decnum は無限大や非数にならないので、比べた結果は CF と ZF だけで決まる。
                asm!(self, "ucomisd %xmm1, %xmm0");
                let set = match op {
                    CmpOp::Lt => "setb",
                    CmpOp::Gt => "seta",
                    CmpOp::Le => "setbe",
                    CmpOp::Ge => "setae",
                    CmpOp::Eq => "sete",
                    CmpOp::Ne => "setne",
                };
                asm!(self, "{} %al", set);
                asm!(self, "movzbl %al, %eax");
                self.store_temp(*dst);
            }
            Inst::IntToDec { dst, a } => {
                self.load(*a, "%rax");
                asm!(self, "cvtsi2sdq %rax, %xmm0");
                asm!(self, "movq %xmm0, %rax");
                self.store_temp(*dst);
            }
            Inst::DecToInt { dst, to, a, span } => {
                let site = self.site_of(*span);
                self.load(*a, "%rax");
                asm!(self, "movq %rax, %xmm0");
                // 切り捨てた結果が範囲に入るか：下限 < 値 < 上限（64 は 下限 <= 値）
                let (lo, hi, lo_inclusive) = match to {
                    IntTy::U8 => (-1.0f64, 256.0f64, false),
                    IntTy::I32 => (-2147483649.0, 2147483648.0, false),
                    IntTy::I64 => (-9223372036854775808.0, 9223372036854775808.0, true),
                };
                asm!(self, "movabsq ${}, %rcx", hi.to_bits() as i64);
                asm!(self, "movq %rcx, %xmm1");
                asm!(self, "ucomisd %xmm1, %xmm0");
                self.trap_jump("jae", site, TrapKind::ConvertDec(*to));
                asm!(self, "movabsq ${}, %rcx", lo.to_bits() as i64);
                asm!(self, "movq %rcx, %xmm1");
                asm!(self, "ucomisd %xmm1, %xmm0");
                self.trap_jump(
                    if lo_inclusive { "jb" } else { "jbe" },
                    site,
                    TrapKind::ConvertDec(*to),
                );
                asm!(self, "cvttsd2siq %xmm0, %rax");
                self.store_temp(*dst);
            }
        }
    }

    /// decnum の二項演算。結果を %rax に（ビットのまま）入れる。
    fn fbin(&mut self, op: FOp, a: Operand, b: Operand, site: Site) {
        self.load(a, "%rax");
        self.load(b, "%rcx");
        if matches!(op, FOp::Div | FOp::Mod) {
            // 0 で割る（-0.0 も同じ）なら止める。
            asm!(self, "movq %rcx, %xmm1");
            asm!(self, "xorpd %xmm2, %xmm2");
            asm!(self, "ucomisd %xmm2, %xmm1");
            self.trap_jump("je", site, TrapKind::Message(Trap::DivZero.message(IntTy::I32)));
        }
        match op {
            FOp::Mod => {
                asm!(self, "movq %rax, %rdi");
                asm!(self, "movq %rcx, %rsi");
                asm!(self, "call orz_rt_fmod");
                return;
            }
            FOp::Pow => {
                let s = self.site(site);
                asm!(self, "movq %rax, %rdi");
                asm!(self, "movq %rcx, %rsi");
                asm!(self, "leaq .Lsite{}(%rip), %rdx", s);
                asm!(self, "call orz_rt_fpow");
                return;
            }
            _ => {}
        }
        asm!(self, "movq %rax, %xmm0");
        asm!(self, "movq %rcx, %xmm1");
        let inst = match op {
            FOp::Add => "addsd",
            FOp::Sub => "subsd",
            FOp::Mul => "mulsd",
            _ => "divsd",
        };
        asm!(self, "{} %xmm1, %xmm0", inst);
        asm!(self, "movq %xmm0, %rax");
        // 指数のビットがすべて 1 なら、無限大か非数になっている。
        asm!(self, "movq %rax, %rcx");
        asm!(self, "shrq $52, %rcx");
        asm!(self, "andl $0x7ff, %ecx");
        asm!(self, "cmpl $0x7ff, %ecx");
        self.trap_jump("je", site, TrapKind::Message(ir::DEC_OVERFLOW.to_string()));
    }

    /// `32` の二項演算。%rax と %rcx から、結果を %rax に（符号を広げて）入れる。
    fn bin32(&mut self, op: IrBinOp, b: Operand, site: Site) {
        let ty = IntTy::I32;
        match op {
            IrBinOp::Add => {
                asm!(self, "addl %ecx, %eax");
                self.trap("jo", site, Trap::Overflow, ty);
            }
            IrBinOp::Sub => {
                asm!(self, "subl %ecx, %eax");
                self.trap("jo", site, Trap::Overflow, ty);
            }
            IrBinOp::Mul => {
                asm!(self, "imull %ecx, %eax");
                self.trap("jo", site, Trap::Overflow, ty);
            }
            IrBinOp::Div | IrBinOp::Mod => {
                self.divmod(op, b, site, ty, ["l", "%eax", "%ecx", "%edx", "cltd"])
            }
            IrBinOp::Shl | IrBinOp::Shr => {
                // 符号なしで比べるので、負の量もここで弾ける。
                asm!(self, "cmpq $31, %rcx");
                self.trap("ja", site, Trap::ShiftRange, ty);
                asm!(
                    self,
                    "{} %cl, %eax",
                    if op == IrBinOp::Shl { "shll" } else { "sarl" }
                );
            }
            IrBinOp::And | IrBinOp::Nand | IrBinOp::Or | IrBinOp::Nor | IrBinOp::Xor | IrBinOp::Xnor => {
                self.bitop(op, "l", "%ecx", "%eax");
            }
            IrBinOp::Pow => unreachable!("累乗はランタイムで計算する"),
        }
        asm!(self, "movslq %eax, %rax");
    }

    /// `64` の二項演算。
    fn bin64(&mut self, op: IrBinOp, b: Operand, site: Site) {
        let ty = IntTy::I64;
        match op {
            IrBinOp::Add => {
                asm!(self, "addq %rcx, %rax");
                self.trap("jo", site, Trap::Overflow, ty);
            }
            IrBinOp::Sub => {
                asm!(self, "subq %rcx, %rax");
                self.trap("jo", site, Trap::Overflow, ty);
            }
            IrBinOp::Mul => {
                asm!(self, "imulq %rcx, %rax");
                self.trap("jo", site, Trap::Overflow, ty);
            }
            IrBinOp::Div | IrBinOp::Mod => {
                self.divmod(op, b, site, ty, ["q", "%rax", "%rcx", "%rdx", "cqto"])
            }
            IrBinOp::Shl | IrBinOp::Shr => {
                asm!(self, "cmpq $63, %rcx");
                self.trap("ja", site, Trap::ShiftRange, ty);
                asm!(
                    self,
                    "{} %cl, %rax",
                    if op == IrBinOp::Shl { "shlq" } else { "sarq" }
                );
            }
            IrBinOp::And | IrBinOp::Nand | IrBinOp::Or | IrBinOp::Nor | IrBinOp::Xor | IrBinOp::Xnor => {
                self.bitop(op, "q", "%rcx", "%rax");
            }
            IrBinOp::Pow => unreachable!("累乗はランタイムで計算する"),
        }
    }

    /// `8`（符号なし）の二項演算。値は 0〜255。
    fn bin8(&mut self, op: IrBinOp, site: Site) {
        let ty = IntTy::U8;
        match op {
            IrBinOp::Add => {
                asm!(self, "addl %ecx, %eax");
                asm!(self, "cmpl $255, %eax");
                self.trap("ja", site, Trap::Overflow, ty);
            }
            IrBinOp::Sub => {
                asm!(self, "subl %ecx, %eax");
                self.trap("jb", site, Trap::Overflow, ty);
            }
            IrBinOp::Mul => {
                asm!(self, "imull %ecx, %eax");
                asm!(self, "cmpl $255, %eax");
                self.trap("ja", site, Trap::Overflow, ty);
            }
            IrBinOp::Div | IrBinOp::Mod => {
                asm!(self, "testl %ecx, %ecx");
                self.trap("jz", site, Trap::DivZero, ty);
                asm!(self, "xorl %edx, %edx");
                asm!(self, "divl %ecx");
                if op == IrBinOp::Mod {
                    asm!(self, "movl %edx, %eax");
                }
            }
            IrBinOp::Shl | IrBinOp::Shr => {
                asm!(self, "cmpq $7, %rcx");
                self.trap("ja", site, Trap::ShiftRange, ty);
                if op == IrBinOp::Shl {
                    asm!(self, "shll %cl, %eax");
                    asm!(self, "movzbl %al, %eax");
                } else {
                    asm!(self, "shrl %cl, %eax");
                }
            }
            IrBinOp::And | IrBinOp::Nand | IrBinOp::Or | IrBinOp::Nor | IrBinOp::Xor | IrBinOp::Xnor => {
                self.bitop(op, "l", "%ecx", "%eax");
                asm!(self, "movzbl %al, %eax");
            }
            IrBinOp::Pow => unreachable!("累乗はランタイムで計算する"),
        }
    }

    fn bitop(&mut self, op: IrBinOp, suffix: &str, rb: &str, ra: &str) {
        let (inst, negate) = match op {
            IrBinOp::And => ("and", false),
            IrBinOp::Nand => ("and", true),
            IrBinOp::Or => ("or", false),
            IrBinOp::Nor => ("or", true),
            IrBinOp::Xor => ("xor", false),
            _ => ("xor", true),
        };
        asm!(self, "{}{} {}, {}", inst, suffix, rb, ra);
        if negate {
            asm!(self, "not{} {}", suffix, ra);
        }
    }

    /// 符号ありの割り算と余り。`r` は（命令の末尾, a, b, 余り, 符号を広げる命令）。
    fn divmod(&mut self, op: IrBinOp, b: Operand, site: Site, ty: IntTy, r: [&str; 5]) {
        let [s, ra, rc, rd, extend] = r;
        let is_div = op == IrBinOp::Div;
        if !matches!(b, Operand::Imm(v) if v != 0) {
            asm!(self, "test{} {}, {}", s, rc, rc);
            self.trap("jz", site, Trap::DivZero, ty);
        }
        let normal = self.new_label();
        let done = self.new_label();
        if !matches!(b, Operand::Imm(v) if v != -1) {
            // -1 で割るとき：最小値 / -1 は溢れる。最小値 % -1 は 0（idiv は例外になるので避ける）。
            asm!(self, "cmp{} $-1, {}", s, rc);
            asm!(self, "jne .L{}", normal);
            if is_div {
                asm!(self, "neg{} {}", s, ra);
                self.trap("jo", site, Trap::Overflow, ty);
            } else {
                asm!(self, "xorl %eax, %eax");
            }
            asm!(self, "jmp .L{}", done);
        }
        let _ = writeln!(self.out, ".L{}:", normal);
        asm!(self, "{}", extend);
        asm!(self, "idiv{} {}", s, rc);
        if !is_div {
            asm!(self, "mov{} {}, {}", s, rd, ra);
        }
        let _ = writeln!(self.out, ".L{}:", done);
    }

    /// 関数を呼ぶ。`extra` は、引数の後ろに足すアドレス（`leaq …` で入れる）。
    /// 引数の 7 個目からは逆順にスタックに積み、呼ぶ時点で %rsp を 16 の倍数にそろえる。
    fn call(&mut self, name: &str, args: &[Operand], extra: &[String]) {
        self.call_with_target(Some(name), None, args, extra);
    }

    /// `name` を呼ぶか、`target`（関数ポインタ）を呼ぶ。
    fn call_with_target(
        &mut self,
        name: Option<&str>,
        target: Option<Operand>,
        args: &[Operand],
        extra: &[String],
    ) {
        let total = args.len() + extra.len();
        assert!(
            extra.is_empty() || total <= ARG_REGS.len(),
            "ランタイムの引数はレジスタに収まる"
        );
        let nstack = total.saturating_sub(ARG_REGS.len());
        let pad = if nstack % 2 == 1 { 8 } else { 0 };
        if pad > 0 {
            asm!(self, "subq $8, %rsp");
        }
        for a in args.iter().skip(ARG_REGS.len()).rev() {
            self.load(*a, "%rax");
            asm!(self, "pushq %rax");
        }
        for (a, reg) in args.iter().zip(ARG_REGS) {
            self.load(*a, reg);
        }
        for (src, reg) in extra.iter().zip(ARG_REGS.iter().skip(args.len())) {
            asm!(self, "leaq {}(%rip), {}", src, reg);
        }
        match (name, target) {
            (Some(name), _) => asm!(self, "call {}", name),
            (None, Some(t)) => {
                // %r11 は引数に使わないレジスタ。
                self.load(t, "%r11");
                asm!(self, "call *%r11");
            }
            (None, None) => unreachable!("呼ぶ先がない"),
        }
        let cleanup = nstack * 8 + pad;
        if cleanup > 0 {
            asm!(self, "addq ${}, %rsp", cleanup);
        }
    }

    /// ランタイムの関数を呼ぶ。必要なら「ファイル名:行」やメッセージを後ろに足す。
    fn call_rt(&mut self, func: Rt, args: &[Operand], span: Span) {
        let site = self.site_of(span);
        let mut args = args.to_vec();
        let mut extra = Vec::new();
        if let Rt::Pow(ty) = func {
            args.push(Operand::Imm(ir::bits_of(ty) as i64));
        }
        if func.takes_site() {
            extra.push(format!(".Lsite{}", self.site(site)));
        }
        if let Rt::Pow(ty) = func {
            extra.push(self.message(Trap::Overflow.message(ty)));
            extra.push(self.message(Trap::NegExp.message(ty)));
        }
        self.call(func.symbol(), &args, &extra);
    }

    fn data(&mut self) {
        let _ = writeln!(self.out, "\n\t.section .rodata");
        // 文字列リテラル：長さ（8 バイト）の後ろに中身と 0。
        for (i, s) in self.prog.strings.iter().enumerate() {
            asm!(self, ".balign 8");
            asm!(self, ".quad {}", s.len());
            let _ = writeln!(self.out, ".Lstr{}:", i);
            self.bytes(s);
        }
        let mut messages: Vec<(u32, String)> = self.messages.iter().map(|(t, n)| (*n, t.clone())).collect();
        messages.sort();
        for (n, text) in messages {
            let _ = writeln!(self.out, ".Lmsg{}:", n);
            self.bytes(text.as_bytes());
        }
        let sites: Vec<(Site, u32)> = self.sites.iter().map(|(s, n)| (*s, *n)).collect();
        for ((file, line), n) in sites {
            let text = format!("{}:{}", self.srcs[file as usize].name, line);
            let _ = writeln!(self.out, ".Lsite{}:", n);
            self.bytes(text.as_bytes());
        }
        // 構造体を表示するための説明と、その表（ランタイムの orz_rt_put が読む）。
        for (i, d) in self.prog.struct_descs.iter().enumerate() {
            let _ = writeln!(self.out, ".Lsd{}:", i);
            self.bytes(d);
        }
        asm!(self, ".balign 8");
        asm!(self, ".globl orz_struct_descs");
        let _ = writeln!(self.out, "orz_struct_descs:");
        for i in 0..self.prog.struct_descs.len() {
            asm!(self, ".quad .Lsd{}", i);
        }
        asm!(self, ".quad 0");

        let _ = writeln!(self.out, "\n\t.data");
        asm!(self, ".balign 8");
        // GC はここからここまでを、グローバル変数として調べる。
        asm!(self, ".globl orz_globals_start");
        let _ = writeln!(self.out, "orz_globals_start:");
        let mut boxes = Vec::new();
        for g in &self.prog.globals {
            let _ = writeln!(self.out, "{}:", g.symbol);
            match g.init {
                ConstVal::Int(v) => asm!(self, ".quad {}", v),
                ConstVal::Str(s) => asm!(self, ".quad .Lstr{}", s),
                ConstVal::Boxed(v) => {
                    asm!(self, ".quad .Lbox{}", boxes.len());
                    boxes.push(v);
                }
                // main の前に、初期化の関数が入れる。
                ConstVal::Deferred => asm!(self, ".quad 0"),
            }
        }
        asm!(self, ".globl orz_globals_end");
        let _ = writeln!(self.out, "orz_globals_end:");
        for (i, v) in boxes.iter().enumerate() {
            let _ = writeln!(self.out, ".Lbox{}:", i);
            asm!(self, ".quad {}", v);
        }
        asm!(self, ".globl orz_gc_disabled");
        let _ = writeln!(self.out, "orz_gc_disabled:");
        asm!(self, ".quad {}", self.gc_disabled as i32);
        // 実行できるスタックは要らない、という印。
        let _ = writeln!(self.out, "\n\t.section .note.GNU-stack,\"\",@progbits");
    }

    /// 0 終端のバイト列。日本語や記号をそのまま入れられるよう、数で書く。
    fn bytes(&mut self, s: &[u8]) {
        let bytes: Vec<String> = s
            .iter()
            .chain(std::iter::once(&0))
            .map(|b| b.to_string())
            .collect();
        for chunk in bytes.chunks(16) {
            asm!(self, ".byte {}", chunk.join(","));
        }
    }
}

/// 命令が使う一時変数（`f(t, 定義か)` で知らせる）。
fn each_temp(inst: &Inst, mut f: impl FnMut(u32, bool)) {
    let use_op = |op: &Operand, f: &mut dyn FnMut(u32, bool)| {
        if let Operand::Temp(t) = op {
            f(*t, false);
        }
    };
    let f: &mut dyn FnMut(u32, bool) = &mut f;
    match inst {
        Inst::Bin { dst, a, b, .. }
        | Inst::Cmp { dst, a, b, .. }
        | Inst::FBin { dst, a, b, .. }
        | Inst::FCmp { dst, a, b, .. } => {
            use_op(a, f);
            use_op(b, f);
            f(*dst, true);
        }
        Inst::Neg { dst, a, .. }
        | Inst::Not { dst, a, .. }
        | Inst::BoolNot { dst, a }
        | Inst::Convert { dst, a, .. }
        | Inst::FNeg { dst, a }
        | Inst::IntToDec { dst, a }
        | Inst::DecToInt { dst, a, .. }
        | Inst::Length { dst, a } => {
            use_op(a, f);
            f(*dst, true);
        }
        Inst::Copy { dst, src } => {
            use_op(src, f);
            f(*dst, true);
        }
        Inst::Store { src, .. } => use_op(src, f),
        Inst::Load { dst, addr, .. } => {
            use_op(addr, f);
            f(*dst, true);
        }
        Inst::StoreMem { addr, src, .. } => {
            use_op(addr, f);
            use_op(src, f);
        }
        Inst::ElemAddr { dst, base, index, .. } => {
            use_op(base, f);
            use_op(index, f);
            f(*dst, true);
        }
        Inst::CheckIndex { array, index, .. } => {
            use_op(array, f);
            use_op(index, f);
        }
        Inst::Call { dst, args, .. } | Inst::CallRt { dst, args, .. } => {
            for a in args {
                use_op(a, f);
            }
            if let Some(d) = dst {
                f(*d, true);
            }
        }
        Inst::CallIndirect { dst, target, args } => {
            use_op(target, f);
            for a in args {
                use_op(a, f);
            }
            if let Some(d) = dst {
                f(*d, true);
            }
        }
        Inst::Branch { cond, .. } => use_op(cond, f),
        Inst::Ret(Some(v)) => use_op(v, f),
        Inst::Ret(None)
        | Inst::Label(_)
        | Inst::Jump(_)
        | Inst::ArenaEnter
        | Inst::ArenaExit
        | Inst::ArenaPop { .. } => {}
    }
}

/// 一時変数に置き場所を割り当てる。使い終わった（最後に使った後の）置き場所は、次の一時変数に回す。
/// 一時変数は 1 つの文の中だけで使われ、ループの先頭に戻る道をまたいで生きることはないので、
/// 命令を前から順に見るだけで、いつ使い終わるかがわかる。（置き場所の番号, 置き場所の数）
fn assign_slots(f: &ir::Function) -> (Vec<u32>, u32) {
    let n = f.ntemps as usize;
    // 番号だけ取って使わなかった一時変数もあるので、出てこないものは None のまま。
    let mut last_use: Vec<Option<usize>> = vec![None; n];
    for (i, inst) in f.insts.iter().enumerate() {
        each_temp(inst, |t, _| {
            last_use[t as usize] = Some(last_use[t as usize].map_or(i, |x| x.max(i)))
        });
    }
    let mut slot = vec![0u32; n];
    let mut free: Vec<u32> = Vec::new();
    let mut nslots = 0u32;
    // その命令の後に空く一時変数
    let mut ends: Vec<Vec<u32>> = vec![Vec::new(); f.insts.len()];
    for (t, i) in last_use.iter().enumerate() {
        if let Some(i) = *i {
            ends[i].push(t as u32);
        }
    }
    for (i, inst) in f.insts.iter().enumerate() {
        each_temp(inst, |t, def| {
            if def {
                slot[t as usize] = free.pop().unwrap_or_else(|| {
                    nslots += 1;
                    nslots - 1
                });
            }
        });
        for &t in &ends[i] {
            free.push(slot[t as usize]);
        }
    }
    (slot, nslots)
}
