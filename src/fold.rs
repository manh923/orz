//! 定数の畳み込み。
//!
//! 値がコンパイル時に決まる計算は、ここで計算してしまう。
//! オーバーフローや 0 除算などがコンパイル時にわかれば、ここでコンパイルエラーにする。

use std::collections::HashMap;

use crate::ast::IntTy;
use crate::ir::{self, Inst, IrBinOp, Operand, Trap};
use crate::source::{Diag, Span};

pub fn fold(prog: &mut ir::Program) -> Result<(), Diag> {
    for f in &mut prog.funcs {
        fold_func(f)?;
    }
    Ok(())
}

fn fold_func(f: &mut ir::Function) -> Result<(), Diag> {
    let mut known: HashMap<u32, i64> = HashMap::new();
    let subst = |known: &HashMap<u32, i64>, op: Operand| match op {
        Operand::Temp(t) => known.get(&t).map(|v| Operand::Imm(*v)).unwrap_or(op),
        imm => imm,
    };
    for inst in &mut f.insts {
        match inst {
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
                if let (Operand::Imm(av), Operand::Imm(bv)) = (*a, *b) {
                    let v = eval_bin(*op, *ty, av, bv).map_err(|t| compile_trap(t, *ty, *span))?;
                    known.insert(*dst, v);
                    *inst = Inst::Copy {
                        dst: *dst,
                        src: Operand::Imm(v),
                    };
                }
            }
            Inst::Neg { dst, ty, a, span } => {
                *a = subst(&known, *a);
                if let Operand::Imm(av) = *a {
                    let v = eval_bin(IrBinOp::Sub, *ty, 0, av).map_err(|t| compile_trap(t, *ty, *span))?;
                    known.insert(*dst, v);
                    *inst = Inst::Copy {
                        dst: *dst,
                        src: Operand::Imm(v),
                    };
                }
            }
            Inst::Not { dst, ty, a } => {
                *a = subst(&known, *a);
                if let Operand::Imm(av) = *a {
                    let v = wrap(*ty, !av);
                    known.insert(*dst, v);
                    *inst = Inst::Copy {
                        dst: *dst,
                        src: Operand::Imm(v),
                    };
                }
            }
            Inst::Copy { dst, src } => {
                *src = subst(&known, *src);
                if let Operand::Imm(v) = *src {
                    known.insert(*dst, v);
                }
            }
            Inst::Ret(Some(v)) => *v = subst(&known, *v),
            Inst::Ret(None) => {}
        }
    }
    Ok(())
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
