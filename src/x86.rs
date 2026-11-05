//! x86-64（Linux）向けのアセンブリ（GNU as、AT&T 記法）を出す。
//!
//! C ライブラリは使わない。`_start` から `main` を呼び、`exit` システムコールで終わる。
//! 一時変数と変数はすべてスタックに置く（速さより、まず正しく動くことを優先）。
//!
//! 関数の呼び出し方は C（System V ABI）と同じ：引数は %edi, %esi, %edx, %ecx, %r8d, %r9d、
//! 7 個目からはスタック、戻り値は %eax。のちに C の関数を呼ぶときも、そのまま使える。

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::ast::IntTy;
use crate::ir::{self, CmpOp, Inst, IrBinOp, Operand, Place, Trap};
use crate::source::SourceFile;

/// 実行時エラーで終わるときの終了コード。
pub const PANIC_EXIT_CODE: i32 = 101;

const ARG_REGS: [&str; 6] = ["%edi", "%esi", "%edx", "%ecx", "%r8d", "%r9d"];

/// スタックが溢れたときに使う、別のスタックの大きさ。
const ALT_STACK_SIZE: u32 = 65536;

pub fn emit(prog: &ir::Program, src: &SourceFile) -> String {
    let mut g = Gen {
        out: String::new(),
        src,
        prog,
        func: 0,
        label: 0,
        sites: BTreeMap::new(),
        traps: Vec::new(),
    };
    g.header();
    for (i, f) in prog.funcs.iter().enumerate() {
        g.func = i;
        g.function(f);
    }
    g.runtime();
    g.data();
    g.out
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
    /// 関数の後ろにまとめて置く、実行時エラーへの飛び先。
    traps: Vec<(u32, usize, Trap)>,
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
        asm!(self, "call __orz_init");
        asm!(self, "call orz_f_{}", main);
        if self.prog.main_returns {
            asm!(self, "movl %eax, %edi");
        } else {
            asm!(self, "xorl %edi, %edi");
        }
        asm!(self, "movl $60, %eax");
        asm!(self, "syscall");
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

    fn operand(&self, op: Operand) -> String {
        match op {
            Operand::Imm(v) => format!("${}", v),
            Operand::Temp(t) => Self::temp_slot(t),
            Operand::Var(v) => self.var_slot(v),
            Operand::Global(g) => self.global(g),
        }
    }

    fn place(&self, p: Place) -> String {
        match p {
            Place::Var(v) => self.var_slot(v),
            Place::Global(g) => self.global(g),
        }
    }

    fn load(&mut self, op: Operand, reg: &str) {
        let s = self.operand(op);
        asm!(self, "movl {}, {}", s, reg);
    }

    fn store_temp(&mut self, dst: u32) {
        asm!(self, "movl %eax, {}", Self::temp_slot(dst));
    }

    fn block_label(&self, l: u32) -> String {
        format!(".Lf{}_{}", self.func, l)
    }

    /// 実行時エラーへ飛ぶ命令を出す。
    fn trap_jump(&mut self, cond: &str, line: usize, trap: Trap) {
        let l = self.new_label();
        self.traps.push((l, line, trap));
        asm!(self, "{} .Ltrap{}", cond, l);
    }

    fn site(&mut self, line: usize) -> u32 {
        let n = self.sites.len() as u32;
        *self.sites.entry(line).or_insert(n)
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
                asm!(self, "movl {}, {}", reg, slot);
            } else {
                // 7 個目からはスタック。16(%rbp) が 7 個目。
                asm!(self, "movl {}(%rbp), %eax", 16 + 8 * (i - 6));
                asm!(self, "movl %eax, {}", slot);
            }
        }
        for inst in &f.insts {
            self.inst(inst);
        }
        if f.returns_value {
            // 値を返す関数は必ず return する（意味解析で確認済み）。
            asm!(self, "ud2");
        }
        // 実行時エラーへの飛び先。
        for (l, line, trap) in std::mem::take(&mut self.traps) {
            let site = self.site(line);
            let _ = writeln!(self.out, ".Ltrap{}:", l);
            asm!(self, "leaq .Lsite{}(%rip), %rdi", site);
            asm!(self, "leaq {}(%rip), %rsi", trap_label(trap));
            asm!(self, "jmp __orz_panic");
        }
    }

    fn inst(&mut self, inst: &Inst) {
        match inst {
            Inst::Copy { dst, src } => {
                self.load(*src, "%eax");
                self.store_temp(*dst);
            }
            Inst::Store { place, src } => {
                self.load(*src, "%eax");
                let p = self.place(*place);
                asm!(self, "movl %eax, {}", p);
            }
            Inst::Label(l) => {
                let _ = writeln!(self.out, "{}:", self.block_label(*l));
            }
            Inst::Jump(l) => {
                let l = self.block_label(*l);
                asm!(self, "jmp {}", l);
            }
            Inst::Branch { cond, then, els } => {
                self.load(*cond, "%eax");
                asm!(self, "testl %eax, %eax");
                let (t, e) = (self.block_label(*then), self.block_label(*els));
                asm!(self, "jnz {}", t);
                asm!(self, "jmp {}", e);
            }
            Inst::Call { dst, func, args } => self.call(*dst, *func, args),
            Inst::Ret(v) => {
                if let Some(v) = v {
                    self.load(*v, "%eax");
                }
                asm!(self, "leave");
                asm!(self, "ret");
            }
            Inst::Cmp { dst, op, a, b } => {
                self.load(*a, "%eax");
                self.load(*b, "%ecx");
                asm!(self, "cmpl %ecx, %eax");
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
                self.load(*a, "%eax");
                asm!(self, "xorl $1, %eax");
                self.store_temp(*dst);
            }
            Inst::Not { dst, ty, a } => {
                assert_eq!(*ty, IntTy::I32);
                self.load(*a, "%eax");
                asm!(self, "notl %eax");
                self.store_temp(*dst);
            }
            Inst::Neg { dst, ty, a, span } => {
                assert_eq!(*ty, IntTy::I32);
                let line = self.src.line_of(span.lo);
                self.load(*a, "%eax");
                asm!(self, "negl %eax");
                self.trap_jump("jo", line, Trap::Overflow);
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
                assert_eq!(*ty, IntTy::I32);
                let line = self.src.line_of(span.lo);
                self.load(*a, "%eax");
                self.load(*b, "%ecx");
                self.bin(*op, *b, line);
                self.store_temp(*dst);
            }
        }
    }

    /// %eax と %ecx の二項演算。結果は %eax。
    fn bin(&mut self, op: IrBinOp, b: Operand, line: usize) {
        match op {
            IrBinOp::Add => {
                asm!(self, "addl %ecx, %eax");
                self.trap_jump("jo", line, Trap::Overflow);
            }
            IrBinOp::Sub => {
                asm!(self, "subl %ecx, %eax");
                self.trap_jump("jo", line, Trap::Overflow);
            }
            IrBinOp::Mul => {
                asm!(self, "imull %ecx, %eax");
                self.trap_jump("jo", line, Trap::Overflow);
            }
            IrBinOp::Div | IrBinOp::Mod => {
                let is_div = op == IrBinOp::Div;
                if !matches!(b, Operand::Imm(v) if v != 0) {
                    asm!(self, "testl %ecx, %ecx");
                    self.trap_jump("jz", line, Trap::DivZero);
                }
                let normal = self.new_label();
                let done = self.new_label();
                if !matches!(b, Operand::Imm(v) if v != -1) {
                    // -1 で割るとき：最小値 / -1 は溢れる。最小値 % -1 は 0（idiv は例外になるので避ける）。
                    asm!(self, "cmpl $-1, %ecx");
                    asm!(self, "jne .L{}", normal);
                    if is_div {
                        asm!(self, "negl %eax");
                        self.trap_jump("jo", line, Trap::Overflow);
                    } else {
                        asm!(self, "xorl %eax, %eax");
                    }
                    asm!(self, "jmp .L{}", done);
                }
                let _ = writeln!(self.out, ".L{}:", normal);
                asm!(self, "cltd");
                asm!(self, "idivl %ecx");
                if !is_div {
                    asm!(self, "movl %edx, %eax");
                }
                let _ = writeln!(self.out, ".L{}:", done);
            }
            IrBinOp::Pow => {
                let site = self.site(line);
                asm!(self, "movl %eax, %edi");
                asm!(self, "movl %ecx, %esi");
                asm!(self, "leaq .Lsite{}(%rip), %rdx", site);
                asm!(self, "call __orz_pow32");
            }
            IrBinOp::Shl | IrBinOp::Shr => {
                // 符号なしで比べるので、負の量もここで弾ける。
                asm!(self, "cmpl $31, %ecx");
                self.trap_jump("ja", line, Trap::ShiftRange);
                if op == IrBinOp::Shl {
                    asm!(self, "shll %cl, %eax");
                } else {
                    asm!(self, "sarl %cl, %eax");
                }
            }
            IrBinOp::And | IrBinOp::Nand => {
                asm!(self, "andl %ecx, %eax");
                if op == IrBinOp::Nand {
                    asm!(self, "notl %eax");
                }
            }
            IrBinOp::Or | IrBinOp::Nor => {
                asm!(self, "orl %ecx, %eax");
                if op == IrBinOp::Nor {
                    asm!(self, "notl %eax");
                }
            }
            IrBinOp::Xor | IrBinOp::Xnor => {
                asm!(self, "xorl %ecx, %eax");
                if op == IrBinOp::Xnor {
                    asm!(self, "notl %eax");
                }
            }
        }
    }

    /// 関数呼び出し。引数の 7 個目からは逆順にスタックに積み、呼ぶ時点で %rsp を 16 の倍数にそろえる。
    fn call(&mut self, dst: Option<u32>, func: u32, args: &[Operand]) {
        let nstack = args.len().saturating_sub(ARG_REGS.len());
        let pad = if nstack % 2 == 1 { 8 } else { 0 };
        if pad > 0 {
            asm!(self, "subq $8, %rsp");
        }
        for a in args.iter().skip(ARG_REGS.len()).rev() {
            self.load(*a, "%eax");
            asm!(self, "pushq %rax");
        }
        for (a, reg) in args.iter().zip(ARG_REGS) {
            self.load(*a, reg);
        }
        let name = &self.prog.funcs[func as usize].name;
        asm!(self, "call orz_f_{}", name);
        let cleanup = nstack * 8 + pad;
        if cleanup > 0 {
            asm!(self, "addq ${}, %rsp", cleanup);
        }
        if let Some(d) = dst {
            self.store_temp(d);
        }
    }

    /// 実行時に使う小さな関数。
    fn runtime(&mut self) {
        self.out.push_str(
            r#"
# ---- ランタイム ----

# 始める前の準備：スタックが溢れたとき（SIGSEGV）に、メッセージを出して止まるようにする。
# 溢れたスタックでは何もできないので、別のスタック（sigaltstack）で受ける。
__orz_init:
	leaq __orz_altstack_desc(%rip), %rdi
	xorl %esi, %esi
	movl $131, %eax          # sigaltstack
	syscall
	movl $11, %edi           # SIGSEGV
	leaq __orz_sigaction(%rip), %rsi
	xorl %edx, %edx
	movl $8, %r10d
	movl $13, %eax           # rt_sigaction
	syscall
	movl $7, %edi            # SIGBUS
	leaq __orz_sigaction(%rip), %rsi
	xorl %edx, %edx
	movl $8, %r10d
	movl $13, %eax
	syscall
	ret

__orz_segv:
	leaq .Lrt_prefix(%rip), %rdi
	call __orz_eputs
	leaq .Lrt_segv(%rip), %rdi
	call __orz_eputs
	leaq .Lrt_nl(%rip), %rdi
	call __orz_eputs
	jmp __orz_exit_panic

# シグナルから戻るとき用（__orz_segv は戻らないので、実際には使われない）
__orz_sigreturn:
	movl $15, %eax
	syscall

# 実行時エラー。%rdi = "ファイル名:行"、%rsi = 理由（どちらも 0 終端）
__orz_panic:
	pushq %rsi
	pushq %rdi
	leaq .Lrt_prefix(%rip), %rdi
	call __orz_eputs
	popq %rdi
	call __orz_eputs
	leaq .Lrt_sep(%rip), %rdi
	call __orz_eputs
	popq %rdi
	call __orz_eputs
	leaq .Lrt_nl(%rip), %rdi
	call __orz_eputs
__orz_exit_panic:
"#,
        );
        let _ = writeln!(self.out, "\tmovl ${}, %edi", PANIC_EXIT_CODE);
        self.out.push_str(
            r#"	movl $60, %eax
	syscall

# 0 終端の文字列を標準エラー出力に書く。%rdi = 文字列
__orz_eputs:
	movq %rdi, %rsi
	xorl %edx, %edx
1:	cmpb $0, (%rsi,%rdx)
	je 2f
	incq %rdx
	jmp 1b
2:	movl $2, %edi
	movl $1, %eax
	syscall
	ret

# 32 の累乗。%edi = 底、%esi = 指数、%rdx = 場所。結果は %eax
# 繰り返し 2 乗法。途中の 2 乗が溢れるなら、最後の結果も溢れる。
__orz_pow32:
	testl %esi, %esi
	js 3f
	movl $1, %eax
	movl %edi, %ecx
1:	testl %esi, %esi
	jz 2f
	testl $1, %esi
	jz 4f
	imull %ecx, %eax
	jo 5f
4:	shrl $1, %esi
	jz 2f
	imull %ecx, %ecx
	jo 5f
	jmp 1b
2:	ret
3:	movq %rdx, %rdi
	leaq .Lrt_negexp(%rip), %rsi
	jmp __orz_panic
5:	movq %rdx, %rdi
	leaq .Lrt_overflow32(%rip), %rsi
	jmp __orz_panic
"#,
        );
    }

    fn data(&mut self) {
        let _ = writeln!(self.out, "\n\t.section .rodata");
        let strings: Vec<(String, String)> = vec![
            (".Lrt_prefix".into(), "実行時エラー: ".into()),
            (".Lrt_sep".into(), ": ".into()),
            (".Lrt_nl".into(), "\n".into()),
            (".Lrt_overflow32".into(), Trap::Overflow.message(IntTy::I32)),
            (".Lrt_divzero".into(), Trap::DivZero.message(IntTy::I32)),
            (".Lrt_negexp".into(), Trap::NegExp.message(IntTy::I32)),
            (".Lrt_shift32".into(), Trap::ShiftRange.message(IntTy::I32)),
            (
                ".Lrt_segv".into(),
                "メモリの読み書きに失敗しました（再帰が深すぎて、スタックが溢れた可能性があります）".into(),
            ),
        ];
        for (label, text) in strings {
            self.cstring(&label, &text);
        }
        let sites: Vec<(usize, u32)> = self.sites.iter().map(|(l, n)| (*l, *n)).collect();
        for (line, n) in sites {
            let text = format!("{}:{}", self.src.name, line);
            self.cstring(&format!(".Lsite{}", n), &text);
        }

        let _ = writeln!(self.out, "\n\t.data");
        asm!(self, ".align 8");
        // stack_t { void *ss_sp; int ss_flags; size_t ss_size; }
        let _ = writeln!(self.out, "__orz_altstack_desc:");
        asm!(self, ".quad __orz_altstack");
        asm!(self, ".long 0, 0");
        asm!(self, ".quad {}", ALT_STACK_SIZE);
        // struct sigaction（カーネル）{ handler; flags; restorer; mask; }
        // flags = SA_ONSTACK | SA_RESTORER
        let _ = writeln!(self.out, "__orz_sigaction:");
        asm!(self, ".quad __orz_segv");
        asm!(self, ".quad 0x0C000000");
        asm!(self, ".quad __orz_sigreturn");
        asm!(self, ".quad 0");
        if !self.prog.globals.is_empty() {
            asm!(self, ".align 4");
            for g in &self.prog.globals {
                let _ = writeln!(self.out, "orz_g_{}:", g.name);
                asm!(self, ".long {}", g.init);
            }
        }
        let _ = writeln!(self.out, "\n\t.bss");
        asm!(self, ".align 16");
        let _ = writeln!(self.out, "__orz_altstack:");
        asm!(self, ".zero {}", ALT_STACK_SIZE);
    }

    /// 0 終端の文字列。日本語や記号をそのまま入れられるよう、バイト列で書く。
    fn cstring(&mut self, label: &str, text: &str) {
        let _ = writeln!(self.out, "{}:", label);
        let bytes: Vec<String> = text
            .bytes()
            .chain(std::iter::once(0))
            .map(|b| b.to_string())
            .collect();
        for chunk in bytes.chunks(16) {
            asm!(self, ".byte {}", chunk.join(","));
        }
    }
}

fn trap_label(t: Trap) -> &'static str {
    match t {
        Trap::Overflow => ".Lrt_overflow32",
        Trap::DivZero => ".Lrt_divzero",
        Trap::NegExp => ".Lrt_negexp",
        Trap::ShiftRange => ".Lrt_shift32",
    }
}
