//! ランタイム（runtime/orz_runtime.c）をコンパイルして、コンパイラに埋め込む。
//! orz を使う人は C コンパイラがなくてもよい（作るときにだけ要る）。
//!
//! orz が動く OS 向けのランタイムは、C コンパイラ（`CC`、なければ `cc`）で作る。
//! ほかの OS 向けのものは、その OS 向けの C コンパイラがあれば作る（なければ空にして、使うときにエラー）。
//!   Linux 向け：`CC_LINUX`、なければ x86_64-linux-gnu-gcc
//!   Windows 向け：`CC_WINDOWS`、なければ x86_64-w64-mingw32-gcc

use std::path::{Path, PathBuf};
use std::process::Command;

const SRC: &str = "runtime/orz_runtime.c";

fn main() {
    println!("cargo:rerun-if-changed={}", SRC);
    for v in ["CC", "CC_LINUX", "CC_WINDOWS"] {
        println!("cargo:rerun-if-env-changed={}", v);
    }
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let host = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    for os in ["linux", "windows"] {
        let out = out_dir.join(format!("orz_runtime_{}.o", os));
        let var = format!("CC_{}", os.to_uppercase());
        if os == host {
            let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
            match compile(&cc, os, &out) {
                Ok(true) => {}
                Ok(false) => panic!("ランタイムのコンパイルに失敗しました（{}）", cc),
                Err(e) => panic!(
                    "C コンパイラ `{}` を実行できません: {}（orz を作るには C コンパイラが必要です）",
                    cc, e
                ),
            }
        } else {
            let cross = match os {
                "linux" => "x86_64-linux-gnu-gcc",
                _ => "x86_64-w64-mingw32-gcc",
            };
            let cc = std::env::var(&var).unwrap_or_else(|_| cross.to_string());
            match compile(&cc, os, &out) {
                Ok(true) => continue,
                Ok(false) => println!(
                    "cargo:warning={} 向けのランタイムのコンパイルに失敗しました（{}）",
                    os, cc
                ),
                // C コンパイラがない。
                Err(_) => {}
            }
            // 作れなかった。空のファイルにしておく。
            std::fs::write(&out, b"").unwrap();
        }
    }
}

fn compile(cc: &str, os: &str, out: &Path) -> std::io::Result<bool> {
    let mut cmd = Command::new(cc);
    cmd.args([
        "-c",
        "-O2",
        "-std=c11",
        "-Wall",
        "-Wextra",
        "-ffreestanding",
        "-fno-builtin",
        "-nostdlib",
        "-fno-stack-protector",
        "-fno-asynchronous-unwind-tables",
        "-fno-tree-loop-distribute-patterns",
        "-fcf-protection=none",
        // GC がスタックを調べるとき、%rsp より下（レッドゾーン）に値が隠れないように。
        "-mno-red-zone",
    ]);
    if os == "windows" {
        // コンパイラが出すアセンブリと同じ呼び出し方（System V ABI）にする。
        cmd.arg("-mabi=sysv");
    } else {
        cmd.arg("-fno-pie");
    }
    let st = cmd.arg("-o").arg(out).arg(SRC).status()?;
    Ok(st.success())
}
