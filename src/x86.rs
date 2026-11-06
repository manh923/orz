//! x86-64（Linux）向けのアセンブリ（GNU as、AT&T 記法）を出す。
//!
//! C ライブラリは使わない。表示や文字列の連結などは、ランタイム（runtime/orz_runtime.c）が受け持つ。
//! 起動の流れ：`_start` → `orz_rt_start`（C）→ `orz_entry`（ここで出す）→ `main`。
//!
//! 一時変数と変数はすべてスタックに置く（速さより、まず正しく動くことを優先）。
//! 値はすべて 8 バイトで持つ。`32` は符号を広げ、`8` と `bool` は 0 を詰めた形にそろえておく。
//!
//! 関数の呼び出し方は C（System V ABI）と同じ：引数は %rdi, %rsi, %rdx, %rcx, %r8, %r9、
//! 7 個目からはスタック、戻り値は %rax。

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use crate::ast::IntTy;
use crate::ir::{self, CmpOp, ConstVal, Inst, IrBinOp, Operand, Place, Rt, Trap, Width};
use crate::source::SourceFile;

const ARG_REGS: [&str; 6] = ["%rdi", "%rsi", "%rdx", "%rcx", "%r8", "%r9"];

pub fn emit(prog: &ir::Program, src: &SourceFile) -> String {
    let mut g = Gen {
        out: String::new(),
        src,
        prog,
        func: 0,
        label: 0,
        sites: BTreeMap::new(),
        messages: HashMap::new(),
        traps: Vec::new(),
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
    /// 変換で範囲外。%rax = 値。
    Convert(IntTy),
}

struct Gen<'a> {
    out: String,
    src: &'a SourceFile,
    prog: &'a ir::Program,
    /// 今出している関数の番号。
    func: usize,
    label: u32,
    /// 実行時エラーで表示する「ファイル名:行」。行番号 → ラベル番号。
    sites: BTreeMap<usize, u32>,
    /// 実行時エラーの理由の文 → ラベル番号。
    messages: HashMap<String, u32>,
    /// 関数の後ろにまとめて置く、実行時エラーへの飛び先。（ラベル, 行, 種類）
    traps: Vec<(u32, usize, TrapKind)>,
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
        let main = &self.prog.funcs[self.prog.main as usize].name;
        let _ = writeln!(self.out, "# orz が出力したアセンブリ（{}）", self.src.name);
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
        asm!(self, "call orz_f_{}", main);
        if !self.prog.main_returns {
            asm!(self, "xorl %eax, %eax");
        }
        asm!(self, "popq %rbp");
        asm!(self, "ret");
    }

    fn cur(&self) -> &'a ir::Function {
        &self.prog.funcs[self.func]
    }

    fn temp_slot(t: u32) -> String {
        format!("-{}(%rbp)", (t + 1) * 8)
    }

    fn var_slot(&self, v: u32) -> String {
        Self::temp_slot(self.cur().ntemps + v)
    }

    fn global(&self, g: u32) -> String {
        format!("orz_g_{}(%rip)", self.prog.globals[g as usize].name)
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
            Operand::Temp(t) => asm!(self, "movq {}, {}", Self::temp_slot(t), reg),
            Operand::Var(v) => {
                let s = self.var_slot(v);
                asm!(self, "movq {}, {}", s, reg)
            }
            Operand::Global(g) => {
                let s = self.global(g);
                asm!(self, "movq {}, {}", s, reg)
            }
            Operand::Str(i) => asm!(self, "leaq .Lstr{}(%rip), {}", i, reg),
        }
    }

    fn store_temp(&mut self, dst: u32) {
        asm!(self, "movq %rax, {}", Self::temp_slot(dst));
    }

    fn block_label(&self, l: u32) -> String {
        format!(".Lf{}_{}", self.func, l)
    }

    fn site(&mut self, line: usize) -> u32 {
        let n = self.sites.len() as u32;
        *self.sites.entry(line).or_insert(n)
    }

    fn message(&mut self, text: String) -> String {
        let n = self.messages.len() as u32;
        format!(".Lmsg{}", *self.messages.entry(text).or_insert(n))
    }

    /// 実行時エラーへ飛ぶ命令を出す。
    fn trap_jump(&mut self, cond: &str, line: usize, kind: TrapKind) {
        let l = self.new_label();
        self.traps.push((l, line, kind));
        asm!(self, "{} .Ltrap{}", cond, l);
    }

    fn trap(&mut self, cond: &str, line: usize, trap: Trap, ty: IntTy) {
        self.trap_jump(cond, line, TrapKind::Message(trap.message(ty)));
    }

    fn function(&mut self, f: &ir::Function) {
        let frame = ((f.ntemps + f.nvars) * 8).div_ceil(16) * 16;
        let _ = writeln!(self.out, "\n# function {}\norz_f_{}:", f.name, f.name);
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
        for (l, line, kind) in std::mem::take(&mut self.traps) {
            let site = self.site(line);
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
                self.load(*index, "%rcx");
                asm!(self, "leaq (%rax,%rcx,{}), %rax", scale);
                self.store_temp(*dst);
            }
            Inst::CheckIndex { array, index, span } => {
                let line = self.src.line_of(span.lo);
                self.load(*array, "%rax");
                asm!(self, "movq -8(%rax), %rdx");
                self.load(*index, "%rcx");
                // 符号なしで比べるので、負の添字もここで弾ける。
                asm!(self, "cmpq %rdx, %rcx");
                self.trap_jump("jae", line, TrapKind::Index);
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
                let name = format!("orz_f_{}", self.prog.funcs[*func as usize].name);
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
                self.call_rt(*func, args, span.lo);
                if let Some(d) = dst {
                    self.store_temp(*d);
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
                let line = self.src.line_of(span.lo);
                self.load(*a, "%rax");
                match ty {
                    IntTy::U8 => {
                        // 符号なしなので、0 以外の符号を変えると範囲の外になる。
                        asm!(self, "testl %eax, %eax");
                        self.trap("jnz", line, Trap::Overflow, *ty);
                    }
                    IntTy::I32 => {
                        asm!(self, "negl %eax");
                        self.trap("jo", line, Trap::Overflow, *ty);
                        asm!(self, "movslq %eax, %rax");
                    }
                    IntTy::I64 => {
                        asm!(self, "negq %rax");
                        self.trap("jo", line, Trap::Overflow, *ty);
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
                let line = self.src.line_of(span.lo);
                self.load(*a, "%rax");
                match (from, to) {
                    (IntTy::I64, IntTy::I32) => {
                        asm!(self, "movslq %eax, %rcx");
                        asm!(self, "cmpq %rax, %rcx");
                        self.trap_jump("jne", line, TrapKind::Convert(*to));
                    }
                    (IntTy::I32 | IntTy::I64, IntTy::U8) => {
                        // 符号なしで比べるので、負の値もここで弾ける。
                        asm!(self, "cmpq $255, %rax");
                        self.trap_jump("ja", line, TrapKind::Convert(*to));
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
                let line = self.src.line_of(span.lo);
                if *op == IrBinOp::Pow {
                    self.pow(*ty, *a, *b, span.lo);
                } else {
                    self.load(*a, "%rax");
                    self.load(*b, "%rcx");
                    match ty {
                        IntTy::I32 => self.bin32(*op, *b, line),
                        IntTy::I64 => self.bin64(*op, *b, line),
                        IntTy::U8 => self.bin8(*op, line),
                    }
                }
                self.store_temp(*dst);
            }
        }
    }

    /// `32` の二項演算。%rax と %rcx から、結果を %rax に（符号を広げて）入れる。
    fn bin32(&mut self, op: IrBinOp, b: Operand, line: usize) {
        let ty = IntTy::I32;
        match op {
            IrBinOp::Add => {
                asm!(self, "addl %ecx, %eax");
                self.trap("jo", line, Trap::Overflow, ty);
            }
            IrBinOp::Sub => {
                asm!(self, "subl %ecx, %eax");
                self.trap("jo", line, Trap::Overflow, ty);
            }
            IrBinOp::Mul => {
                asm!(self, "imull %ecx, %eax");
                self.trap("jo", line, Trap::Overflow, ty);
            }
            IrBinOp::Div | IrBinOp::Mod => self.divmod(op, b, line, ty, "l", "%eax", "%ecx", "%edx", "cltd"),
            IrBinOp::Shl | IrBinOp::Shr => {
                // 符号なしで比べるので、負の量もここで弾ける。
                asm!(self, "cmpq $31, %rcx");
                self.trap("ja", line, Trap::ShiftRange, ty);
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
    fn bin64(&mut self, op: IrBinOp, b: Operand, line: usize) {
        let ty = IntTy::I64;
        match op {
            IrBinOp::Add => {
                asm!(self, "addq %rcx, %rax");
                self.trap("jo", line, Trap::Overflow, ty);
            }
            IrBinOp::Sub => {
                asm!(self, "subq %rcx, %rax");
                self.trap("jo", line, Trap::Overflow, ty);
            }
            IrBinOp::Mul => {
                asm!(self, "imulq %rcx, %rax");
                self.trap("jo", line, Trap::Overflow, ty);
            }
            IrBinOp::Div | IrBinOp::Mod => self.divmod(op, b, line, ty, "q", "%rax", "%rcx", "%rdx", "cqto"),
            IrBinOp::Shl | IrBinOp::Shr => {
                asm!(self, "cmpq $63, %rcx");
                self.trap("ja", line, Trap::ShiftRange, ty);
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
    fn bin8(&mut self, op: IrBinOp, line: usize) {
        let ty = IntTy::U8;
        match op {
            IrBinOp::Add => {
                asm!(self, "addl %ecx, %eax");
                asm!(self, "cmpl $255, %eax");
                self.trap("ja", line, Trap::Overflow, ty);
            }
            IrBinOp::Sub => {
                asm!(self, "subl %ecx, %eax");
                self.trap("jb", line, Trap::Overflow, ty);
            }
            IrBinOp::Mul => {
                asm!(self, "imull %ecx, %eax");
                asm!(self, "cmpl $255, %eax");
                self.trap("ja", line, Trap::Overflow, ty);
            }
            IrBinOp::Div | IrBinOp::Mod => {
                asm!(self, "testl %ecx, %ecx");
                self.trap("jz", line, Trap::DivZero, ty);
                asm!(self, "xorl %edx, %edx");
                asm!(self, "divl %ecx");
                if op == IrBinOp::Mod {
                    asm!(self, "movl %edx, %eax");
                }
            }
            IrBinOp::Shl | IrBinOp::Shr => {
                asm!(self, "cmpq $7, %rcx");
                self.trap("ja", line, Trap::ShiftRange, ty);
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

    /// 符号ありの割り算と余り。
    #[allow(clippy::too_many_arguments)]
    fn divmod(
        &mut self,
        op: IrBinOp,
        b: Operand,
        line: usize,
        ty: IntTy,
        s: &str,
        ra: &str,
        rc: &str,
        rd: &str,
        extend: &str,
    ) {
        let is_div = op == IrBinOp::Div;
        if !matches!(b, Operand::Imm(v) if v != 0) {
            asm!(self, "test{} {}, {}", s, rc, rc);
            self.trap("jz", line, Trap::DivZero, ty);
        }
        let normal = self.new_label();
        let done = self.new_label();
        if !matches!(b, Operand::Imm(v) if v != -1) {
            // -1 で割るとき：最小値 / -1 は溢れる。最小値 % -1 は 0（idiv は例外になるので避ける）。
            asm!(self, "cmp{} $-1, {}", s, rc);
            asm!(self, "jne .L{}", normal);
            if is_div {
                asm!(self, "neg{} {}", s, ra);
                self.trap("jo", line, Trap::Overflow, ty);
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

    /// 累乗はランタイムで計算する。
    fn pow(&mut self, ty: IntTy, a: Operand, b: Operand, pos: usize) {
        self.call_rt(Rt::Pow(ty), &[a, b], pos);
    }

    /// 関数を呼ぶ。`extra` は、引数の後ろに足すレジスタの準備（`leaq …` など）。
    /// 引数の 7 個目からは逆順にスタックに積み、呼ぶ時点で %rsp を 16 の倍数にそろえる。
    fn call(&mut self, name: &str, args: &[Operand], extra: &[String]) {
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
        asm!(self, "call {}", name);
        let cleanup = nstack * 8 + pad;
        if cleanup > 0 {
            asm!(self, "addq ${}, %rsp", cleanup);
        }
    }

    /// ランタイムの関数を呼ぶ。必要なら「ファイル名:行」やメッセージを後ろに足す。
    fn call_rt(&mut self, func: Rt, args: &[Operand], pos: usize) {
        let line = self.src.line_of(pos);
        let mut args = args.to_vec();
        let mut extra = Vec::new();
        if let Rt::Pow(ty) = func {
            args.push(Operand::Imm(ir::bits_of(ty) as i64));
        }
        if func.takes_site() {
            extra.push(format!(".Lsite{}", self.site(line)));
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
        let sites: Vec<(usize, u32)> = self.sites.iter().map(|(l, n)| (*l, *n)).collect();
        for (line, n) in sites {
            let text = format!("{}:{}", self.src.name, line);
            let _ = writeln!(self.out, ".Lsite{}:", n);
            self.bytes(text.as_bytes());
        }
        if !self.prog.globals.is_empty() {
            let _ = writeln!(self.out, "\n\t.data");
            asm!(self, ".balign 8");
            for g in &self.prog.globals {
                let _ = writeln!(self.out, "orz_g_{}:", g.name);
                match g.init {
                    ConstVal::Int(v) => asm!(self, ".quad {}", v),
                    ConstVal::Str(s) => asm!(self, ".quad .Lstr{}", s),
                }
            }
        }
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
