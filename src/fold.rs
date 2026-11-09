//! 定数の畳み込み。
//!
//! 値がコンパイル時に決まる計算は、ここで計算してしまう。
//! オーバーフローや 0 除算などがコンパイル時にわかれば、ここでコンパイルエラーにする。
//!
//! 一時変数は 1 回だけ代入され、使う場所より前で必ず代入されているので、
//! 前から順に見て「値のわかった一時変数」を覚えていくだけでよい。
//! 変数（`Var`・`Global`）は何度も書き換わるので、値がわかったものとしては扱わない。

use std::collections::HashMap;

use crate::ast::IntTy;
use crate::ir::{self, CmpOp, ConstVal, FOp, Inst, IrBinOp, Operand, Trap};
use crate::source::{Diag, Span};

pub fn fold(prog: &mut ir::Program) -> Result<(), Diag> {
    for f in &mut prog.funcs {
        fold_insts(&mut f.insts)?;
    }
    Ok(())
}

/// グローバル変数の初期値の計算。`result` の値がコンパイル時にわかれば返す。
pub fn fold_const(insts: &mut [Inst], result: Operand) -> Result<Option<ConstVal>, Diag> {
    let known = fold_insts(insts)?;
    Ok(match result {
        Operand::Imm(v) => Some(ConstVal::Int(v)),
        Operand::Str(s) => Some(ConstVal::Str(s)),
        Operand::Temp(t) => known.get(&t).copied().map(ConstVal::Int),
        _ => None,
    })
}

fn subst(known: &HashMap<u32, i64>, op: Operand) -> Operand {
    match op {
        Operand::Temp(t) => known.get(&t).map(|v| Operand::Imm(*v)).unwrap_or(op),
        other => other,
    }
}

/// 命令の置き換え方。
enum Repl {
    /// 値がわかった：`dst = 値` にする。
    Const(u32, i64),
    /// 分岐の向きがわかった：`goto` にする。
    Jump(u32),
}

fn fold_insts(insts: &mut [Inst]) -> Result<HashMap<u32, i64>, Diag> {
    let mut known: HashMap<u32, i64> = HashMap::new();
    for inst in insts.iter_mut() {
        let replaced = match inst {
            Inst::Bin {
                dst,
                op,
                ty,
                a,
                b,
                span,
            } => {
                *a = subst(&known, *a);
                *b = subst(&known, *b);
                // 右辺だけでわかるもの（0 で割る、など）。
                if let Operand::Imm(bv) = *b {
                    check_rhs(*op, *ty, bv, *span)?;
                }
                match (*a, *b) {
                    (Operand::Imm(av), Operand::Imm(bv)) => {
                        let v = eval_bin(*op, *ty, av, bv).map_err(|t| compile_trap(t, *ty, *span))?;
                        Some(Repl::Const(*dst, v))
                    }
                    _ => None,
                }
            }
            Inst::Neg { dst, ty, a, span } => {
                *a = subst(&known, *a);
                match *a {
                    Operand::Imm(av) => {
                        let v =
                            eval_bin(IrBinOp::Sub, *ty, 0, av).map_err(|t| compile_trap(t, *ty, *span))?;
                        Some(Repl::Const(*dst, v))
                    }
                    _ => None,
                }
            }
            Inst::Not { dst, ty, a } => {
                *a = subst(&known, *a);
                match *a {
                    Operand::Imm(av) => Some(Repl::Const(*dst, wrap(*ty, !av))),
                    _ => None,
                }
            }
            Inst::BoolNot { dst, a } => {
                *a = subst(&known, *a);
                match *a {
                    Operand::Imm(av) => Some(Repl::Const(*dst, (av == 0) as i64)),
                    _ => None,
                }
            }
            Inst::Cmp { dst, op, a, b } => {
                *a = subst(&known, *a);
                *b = subst(&known, *b);
                match (*a, *b) {
                    (Operand::Imm(av), Operand::Imm(bv)) => {
                        Some(Repl::Const(*dst, eval_cmp(*op, av, bv) as i64))
                    }
                    _ => None,
                }
            }
            Inst::Convert {
                dst,
                from: _,
                to,
                a,
                span,
            } => {
                *a = subst(&known, *a);
                match *a {
                    Operand::Imm(av) => {
                        if av < ir::min_of(*to) || av > ir::max_of(*to) {
                            return Err(Diag::new(
                                *span,
                                format!(
                                    "コンパイル時にわかる誤り：変換する値 {} が {}",
                                    av,
                                    ir::convert_message(*to)
                                ),
                            ));
                        }
                        Some(Repl::Const(*dst, av))
                    }
                    _ => None,
                }
            }
            Inst::FBin { dst, op, a, b, span } => {
                *a = subst(&known, *a);
                *b = subst(&known, *b);
                if let Operand::Imm(bv) = *b {
                    if matches!(op, FOp::Div | FOp::Mod) && f64::from_bits(bv as u64) == 0.0 {
                        return Err(compile_err(*span, "0 で割りました"));
                    }
                }
                match (*a, *b) {
                    // 累乗は、実行時と同じ結果にするためランタイムに任せる。
                    (Operand::Imm(av), Operand::Imm(bv)) if *op != FOp::Pow => {
                        let (x, y) = (f64::from_bits(av as u64), f64::from_bits(bv as u64));
                        let r = match op {
                            FOp::Add => x + y,
                            FOp::Sub => x - y,
                            FOp::Mul => x * y,
                            FOp::Div => x / y,
                            // Rust の % は C の fmod と同じ（割られる数の符号、誤差なし）。
                            FOp::Mod => x % y,
                            FOp::Pow => unreachable!(),
                        };
                        if !r.is_finite() {
                            return Err(compile_err(*span, ir::DEC_OVERFLOW));
                        }
                        Some(Repl::Const(*dst, r.to_bits() as i64))
                    }
                    _ => None,
                }
            }
            Inst::FNeg { dst, a } => {
                *a = subst(&known, *a);
                match *a {
                    Operand::Imm(av) => Some(Repl::Const(*dst, ((av as u64) ^ (1u64 << 63)) as i64)),
                    _ => None,
                }
            }
            Inst::FCmp { dst, op, a, b } => {
                *a = subst(&known, *a);
                *b = subst(&known, *b);
                match (*a, *b) {
                    (Operand::Imm(av), Operand::Imm(bv)) => {
                        let (x, y) = (f64::from_bits(av as u64), f64::from_bits(bv as u64));
                        let r = match op {
                            CmpOp::Lt => x < y,
                            CmpOp::Gt => x > y,
                            CmpOp::Le => x <= y,
                            CmpOp::Ge => x >= y,
                            CmpOp::Eq => x == y,
                            CmpOp::Ne => x != y,
                        };
                        Some(Repl::Const(*dst, r as i64))
                    }
                    _ => None,
                }
            }
            Inst::IntToDec { dst, a } => {
                *a = subst(&known, *a);
                match *a {
                    Operand::Imm(av) => Some(Repl::Const(*dst, (av as f64).to_bits() as i64)),
                    _ => None,
                }
            }
            Inst::DecToInt { dst, to, a, span } => {
                *a = subst(&known, *a);
                match *a {
                    Operand::Imm(av) => {
                        let x = f64::from_bits(av as u64);
                        if !ir::dec_fits(x, *to) {
                            return Err(compile_err(
                                *span,
                                &format!("変換する値 {} が {}", x, ir::convert_message(*to)),
                            ));
                        }
                        Some(Repl::Const(*dst, x.trunc() as i64))
                    }
                    _ => None,
                }
            }
            Inst::Copy { dst, src } => {
                *src = subst(&known, *src);
                if let Operand::Imm(v) = *src {
                    known.insert(*dst, v);
                }
                None
            }
            Inst::Store { src, .. } => {
                *src = subst(&known, *src);
                None
            }
            Inst::Load { addr, .. } => {
                *addr = subst(&known, *addr);
                None
            }
            Inst::StoreMem { addr, src, .. } => {
                *addr = subst(&known, *addr);
                *src = subst(&known, *src);
                None
            }
            Inst::ElemAddr { base, index, .. } => {
                *base = subst(&known, *base);
                *index = subst(&known, *index);
                None
            }
            Inst::CheckIndex { array, index, .. } => {
                *array = subst(&known, *array);
                *index = subst(&known, *index);
                None
            }
            Inst::Length { a, .. } => {
                *a = subst(&known, *a);
                None
            }
            Inst::Call { args, .. } | Inst::CallRt { args, .. } => {
                for a in args.iter_mut() {
                    *a = subst(&known, *a);
                }
                None
            }
            Inst::CallIndirect { target, args, .. } => {
                *target = subst(&known, *target);
                for a in args.iter_mut() {
                    *a = subst(&known, *a);
                }
                None
            }
            Inst::Syscall { args, .. } => {
                for a in args.iter_mut() {
                    *a = subst(&known, *a);
                }
                None
            }
            Inst::Branch { cond, then, els } => {
                *cond = subst(&known, *cond);
                match *cond {
                    Operand::Imm(c) => Some(Repl::Jump(if c != 0 { *then } else { *els })),
                    _ => None,
                }
            }
            Inst::Ret(Some(v)) => {
                *v = subst(&known, *v);
                None
            }
            Inst::Ret(None)
            | Inst::Label(_)
            | Inst::Jump(_)
            | Inst::ArenaEnter
            | Inst::ArenaExit
            | Inst::ArenaPop { .. } => None,
        };
        match replaced {
            Some(Repl::Const(dst, v)) => {
                known.insert(dst, v);
                *inst = Inst::Copy {
                    dst,
                    src: Operand::Imm(v),
                };
            }
            Some(Repl::Jump(l)) => *inst = Inst::Jump(l),
            None => {}
        }
    }
    Ok(known)
}

fn compile_err(span: Span, msg: &str) -> Diag {
    Diag::new(span, format!("コンパイル時にわかる誤り：{}", msg))
}

fn compile_trap(t: Trap, ty: IntTy, span: Span) -> Diag {
    Diag::new(span, format!("コンパイル時にわかる誤り：{}", t.message(ty)))
}

/// 右辺の値だけで誤りとわかるか。
fn check_rhs(op: IrBinOp, ty: IntTy, b: i64, span: Span) -> Result<(), Diag> {
    let trap = match op {
        IrBinOp::Div | IrBinOp::Mod if b == 0 => Trap::DivZero,
        IrBinOp::Pow if b < 0 => Trap::NegExp,
        IrBinOp::Shl | IrBinOp::Shr if b < 0 || b >= ir::bits_of(ty) as i64 => Trap::ShiftRange,
        _ => return Ok(()),
    };
    Err(compile_trap(trap, ty, span))
}

/// 型の幅に切り詰める（ビット演算の結果用）。
fn wrap(ty: IntTy, v: i64) -> i64 {
    match ty {
        IntTy::U8 => v as u8 as i64,
        IntTy::I32 => v as i32 as i64,
        IntTy::I64 => v,
    }
}

fn in_range(ty: IntTy, v: i128) -> Result<i64, Trap> {
    if v < ir::min_of(ty) as i128 || v > ir::max_of(ty) as i128 {
        Err(Trap::Overflow)
    } else {
        Ok(v as i64)
    }
}

pub fn eval_cmp(op: CmpOp, a: i64, b: i64) -> bool {
    match op {
        CmpOp::Lt => a < b,
        CmpOp::Gt => a > b,
        CmpOp::Le => a <= b,
        CmpOp::Ge => a >= b,
        CmpOp::Eq => a == b,
        CmpOp::Ne => a != b,
    }
}

/// 実行時と同じ決まりで計算する。
pub fn eval_bin(op: IrBinOp, ty: IntTy, a: i64, b: i64) -> Result<i64, Trap> {
    let (a, b) = (a as i128, b as i128);
    let bits = ir::bits_of(ty);
    match op {
        IrBinOp::Add => in_range(ty, a + b),
        IrBinOp::Sub => in_range(ty, a - b),
        IrBinOp::Mul => in_range(ty, a * b),
        IrBinOp::Div | IrBinOp::Mod => {
            if b == 0 {
                return Err(Trap::DivZero);
            }
            // 0 の方向に切り捨て。余りの符号は割られる数と同じ（C と同じ）。
            if op == IrBinOp::Div {
                in_range(ty, a / b)
            } else {
                in_range(ty, a % b)
            }
        }
        IrBinOp::Pow => {
            if b < 0 {
                return Err(Trap::NegExp);
            }
            let mut r: i128 = 1;
            let mut base = a;
            let mut e = b;
            // 繰り返し 2 乗法。途中の 2 乗が溢れるなら、最後の結果も溢れる（底が 0・±1 なら溢れない）。
            while e > 0 {
                if e & 1 == 1 {
                    r = in_range(ty, r * base)? as i128;
                }
                e >>= 1;
                if e > 0 {
                    base = in_range(ty, base * base)? as i128;
                }
            }
            Ok(r as i64)
        }
        IrBinOp::Shl | IrBinOp::Shr => {
            if b < 0 || b >= bits as i128 {
                return Err(Trap::ShiftRange);
            }
            let (a, b) = (a as i64, b as u32);
            // `<<` はビット演算なのでオーバーフローは調べない。`>>` は符号ありなら算術シフト。
            Ok(match (op, ty) {
                (IrBinOp::Shl, _) => wrap(ty, a.wrapping_shl(b)),
                (_, IntTy::U8) => ((a as u8) >> b) as i64,
                (_, IntTy::I32) => ((a as i32) >> b) as i64,
                (_, IntTy::I64) => a >> b,
            })
        }
        IrBinOp::And => Ok(wrap(ty, (a & b) as i64)),
        IrBinOp::Nand => Ok(wrap(ty, !(a & b) as i64)),
        IrBinOp::Or => Ok(wrap(ty, (a | b) as i64)),
        IrBinOp::Nor => Ok(wrap(ty, !(a | b) as i64)),
        IrBinOp::Xor => Ok(wrap(ty, (a ^ b) as i64)),
        IrBinOp::Xnor => Ok(wrap(ty, !(a ^ b) as i64)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: IntTy = IntTy::I32;

    #[test]
    fn arithmetic() {
        assert_eq!(eval_bin(IrBinOp::Add, T, 1, 2), Ok(3));
        assert_eq!(eval_bin(IrBinOp::Add, T, i32::MAX as i64, 1), Err(Trap::Overflow));
        assert_eq!(eval_bin(IrBinOp::Div, T, 7, -2), Ok(-3));
        assert_eq!(eval_bin(IrBinOp::Mod, T, -7, 2), Ok(-1));
        assert_eq!(
            eval_bin(IrBinOp::Div, T, i32::MIN as i64, -1),
            Err(Trap::Overflow)
        );
        assert_eq!(eval_bin(IrBinOp::Mod, T, i32::MIN as i64, -1), Ok(0));
        assert_eq!(eval_bin(IrBinOp::Div, T, 1, 0), Err(Trap::DivZero));
    }

    #[test]
    fn power() {
        assert_eq!(eval_bin(IrBinOp::Pow, T, 2, 10), Ok(1024));
        assert_eq!(eval_bin(IrBinOp::Pow, T, -2, 31), Ok(i32::MIN as i64));
        assert_eq!(eval_bin(IrBinOp::Pow, T, 2, 31), Err(Trap::Overflow));
        assert_eq!(eval_bin(IrBinOp::Pow, T, 1, i32::MAX as i64), Ok(1));
        assert_eq!(eval_bin(IrBinOp::Pow, T, -1, 3), Ok(-1));
        assert_eq!(eval_bin(IrBinOp::Pow, T, 0, 0), Ok(1));
        assert_eq!(eval_bin(IrBinOp::Pow, T, 2, -1), Err(Trap::NegExp));
    }

    #[test]
    fn bits() {
        assert_eq!(eval_bin(IrBinOp::Shl, T, 1, 31), Ok(i32::MIN as i64));
        assert_eq!(eval_bin(IrBinOp::Shr, T, -8, 1), Ok(-4));
        assert_eq!(eval_bin(IrBinOp::Shl, T, 1, 32), Err(Trap::ShiftRange));
        assert_eq!(eval_bin(IrBinOp::Xnor, T, 0, 0), Ok(-1));
        assert_eq!(eval_bin(IrBinOp::Nand, T, 12, 10), Ok(!8));
    }
}
