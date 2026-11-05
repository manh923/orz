//! x86-64（Linux）向けのアセンブリ（GNU as、AT&T 記法）を出す。
//!
//! C ライブラリは使わない。`_start` から `main` を呼び、`exit` システムコールで終わる。
//! 一時変数はすべてスタックに置く（速さより、まず正しく動くことを優先）。

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::ast::IntTy;
use crate::ir::{self, Inst, IrBinOp, Operand, Trap};
use crate::source::SourceFile;

/// 実行時エラーで終わるときの終了コード。
pub const PANIC_EXIT_CODE: i32 = 101;

pub fn emit(prog: &ir::Program, src: &SourceFile) -> String {
    let mut g = Gen {
        out: String::new(),
        src,
        label: 0,
        sites: BTreeMap::new(),
        traps: Vec::new(),
    };
    g.header(prog);
    for f in &prog.funcs {
        g.func(f);
    }
    g.runtime();
    g.data();
    g.out
}

struct Gen<'a> {
    out: String,
    src: &'a SourceFile,
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

    fn header(&mut self, prog: &ir::Program) {
        let _ = writeln!(self.out, "# orz が出力したアセンブリ（{}）", self.src.name);
        asm!(self, ".text");
        asm!(self, ".globl _start");
        let _ = writeln!(self.out, "_start:");
        asm!(self, "xorl %ebp, %ebp");
        asm!(self, "call orz_main");
        if prog.main_returns {
            asm!(self, "movl %eax, %edi");
        } else {
            asm!(self, "xorl %edi, %edi");
        }
        asm!(self, "movl $60, %eax");
        asm!(self, "syscall");
    }

    fn slot(t: u32) -> String {
        format!("-{}(%rbp)", (t + 1) * 8)
    }

    fn operand(&self, op: Operand) -> String {
        match op {
            Operand::Imm(v) => format!("${}", v),
            Operand::Temp(t) => Self::slot(t),
        }
    }

    fn load(&mut self, op: Operand, reg: &str) {
        let s = self.operand(op);
        asm!(self, "movl {}, {}", s, reg);
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

    fn func(&mut self, f: &ir::Function) {
        let frame = (f.ntemps * 8).div_ceil(16) * 16;
        let _ = writeln!(self.out, "\norz_{}:", f.name);
        asm!(self, "pushq %rbp");
        asm!(self, "movq %rsp, %rbp");
        if frame > 0 {
            asm!(self, "subq ${}, %rsp", frame);
        }
        for inst in &f.insts {
            self.inst(inst);
        }
        if f.ret.is_some() {
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
                asm!(self, "movl %eax, {}", Self::slot(*dst));
            }
            Inst::Ret(v) => {
                if let Some(v) = v {
                    self.load(*v, "%eax");
                }
                asm!(self, "leave");
                asm!(self, "ret");
            }
            Inst::Not { dst, ty, a } => {
                assert_eq!(*ty, IntTy::I32);
                self.load(*a, "%eax");
                asm!(self, "notl %eax");
                asm!(self, "movl %eax, {}", Self::slot(*dst));
            }
            Inst::Neg { dst, ty, a, span } => {
                assert_eq!(*ty, IntTy::I32);
                let line = self.src.line_of(span.lo);
                self.load(*a, "%eax");
                asm!(self, "negl %eax");
                self.trap_jump("jo", line, Trap::Overflow);
                asm!(self, "movl %eax, {}", Self::slot(*dst));
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
                        let is_div = *op == IrBinOp::Div;
                        if !matches!(b, Operand::Imm(v) if *v != 0) {
                            asm!(self, "testl %ecx, %ecx");
                            self.trap_jump("jz", line, Trap::DivZero);
                        }
                        let normal = self.new_label();
                        let done = self.new_label();
                        if !matches!(b, Operand::Imm(v) if *v != -1) {
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
                        if *op == IrBinOp::Shl {
                            asm!(self, "shll %cl, %eax");
                        } else {
                            asm!(self, "sarl %cl, %eax");
                        }
                    }
                    IrBinOp::And | IrBinOp::Nand => {
                        asm!(self, "andl %ecx, %eax");
                        if *op == IrBinOp::Nand {
                            asm!(self, "notl %eax");
                        }
                    }
                    IrBinOp::Or | IrBinOp::Nor => {
                        asm!(self, "orl %ecx, %eax");
                        if *op == IrBinOp::Nor {
                            asm!(self, "notl %eax");
                        }
                    }
                    IrBinOp::Xor | IrBinOp::Xnor => {
                        asm!(self, "xorl %ecx, %eax");
                        if *op == IrBinOp::Xnor {
                            asm!(self, "notl %eax");
                        }
                    }
                }
                asm!(self, "movl %eax, {}", Self::slot(*dst));
            }
        }
    }

    /// 実行時に使う小さな関数。
    fn runtime(&mut self) {
        self.out.push_str(
            r#"
# ---- ランタイム ----

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
        ];
        for (label, text) in strings {
            self.cstring(&label, &text);
        }
        let sites: Vec<(usize, u32)> = self.sites.iter().map(|(l, n)| (*l, *n)).collect();
        for (line, n) in sites {
            let text = format!("{}:{}", self.src.name, line);
            self.cstring(&format!(".Lsite{}", n), &text);
        }
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
