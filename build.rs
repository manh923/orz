//! ランタイム（runtime/orz_runtime.c）をコンパイルして、コンパイラに埋め込む。
//! orz を使う人は C コンパイラがなくてもよい（作るときにだけ要る）。

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let src = "runtime/orz_runtime.c";
    println!("cargo:rerun-if-changed={}", src);
    println!("cargo:rerun-if-env-changed=CC");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("orz_runtime.o");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let status = Command::new(&cc)
        .args([
            "-c",
            "-O2",
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-ffreestanding",
            "-fno-builtin",
            "-nostdlib",
            "-fno-stack-protector",
            "-fno-pie",
            "-fno-asynchronous-unwind-tables",
            "-fno-tree-loop-distribute-patterns",
            "-fcf-protection=none",
            // GC がスタックを調べるとき、%rsp より下（レッドゾーン）に値が隠れないように。
            "-mno-red-zone",
            "-o",
        ])
        .arg(&out)
        .arg(src)
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => panic!("ランタイムのコンパイルに失敗しました（{} {}）", cc, s),
        Err(e) => panic!(
            "C コンパイラ `{}` を実行できません: {}（orz を作るには C コンパイラが必要です）",
            cc, e
        ),
    }
}
