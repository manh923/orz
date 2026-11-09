//! tests/cases/*.orz をすべてコンパイルして試す。
//!
//! ファイルの先頭のコメントで、期待する結果を書く。
//!   // exit: 3          正しく動いて、終了コードが 3。定数の畳み込みあり・なしの両方で試す
//!   // error: 文字列     コンパイルエラーになり、その文字列が表示される
//!   // runtime: 文字列   実行時エラーになり、その文字列が表示される
//!   // flags: --no-fold   定数の畳み込みを切ってコンパイルする（実行時の検査を試すため）。ほかのオプションも書ける
//!   // args: a b c       実行するときのコマンドライン引数
//!   // os: linux         その OS 向けのときだけ試す（linux か windows）
//!
//! 同じ名前の `.out` があれば、標準出力がその中身とぴったり同じかを確かめる。
//! 同じ名前の `.in` があれば、それを標準入力に流す。
//!
//! `ORZ_TEST_TARGET=windows` なら、Windows 向けに作って wine で動かす（Linux の上で Windows 版を試すため）。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const PANIC_EXIT_CODE: i32 = 101;

fn orz() -> &'static str {
    env!("CARGO_BIN_EXE_orz")
}

/// Linux の上で、Windows 向けに作って wine で動かすか。
fn on_wine() -> bool {
    !cfg!(windows) && std::env::var("ORZ_TEST_TARGET").is_ok_and(|t| t == "windows")
}

fn header(src: &str, key: &str) -> Option<String> {
    src.lines().take_while(|l| l.starts_with("//")).find_map(|l| {
        l.strip_prefix("//")
            .unwrap()
            .trim()
            .strip_prefix(key)
            .map(|v| v.trim().to_string())
    })
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

/// 実行ファイルを作って実行する。コンパイルエラーなら Err（標準エラー出力）。
fn build_and_run(case: &Path, dir: &Path, no_fold: bool, src: &str) -> Result<Run, String> {
    let stem = case.file_stem().unwrap().to_str().unwrap();
    let suffix = if on_wine() {
        ".exe"
    } else {
        std::env::consts::EXE_SUFFIX
    };
    let exe = dir.join(format!(
        "{}{}{}",
        stem,
        if no_fold { "-nofold" } else { "" },
        suffix
    ));
    let mut cmd = Command::new(orz());
    cmd.current_dir(case.parent().unwrap())
        .arg("build")
        .arg(case.file_name().unwrap())
        .arg("-o")
        .arg(&exe);
    if no_fold {
        cmd.arg("--no-fold");
    }
    if on_wine() {
        cmd.args(["--target", "windows"]);
    }
    // `// flags:` に書いたほかのオプション（--no-gc など）
    for f in header(src, "flags:").unwrap_or_default().split_whitespace() {
        if f != "--no-fold" {
            cmd.arg(f);
        }
    }
    let out = cmd.output().unwrap();
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    // ファイルを書くテストのため、実行はテストごとの空のフォルダで行う。
    let work = dir.join(format!("work-{}", stem));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let args: Vec<String> = header(src, "args:")
        .map(|a| a.split_whitespace().map(String::from).collect())
        .unwrap_or_default();
    let input = std::fs::read(case.with_extension("in")).unwrap_or_default();
    let mut run = if on_wine() {
        let mut c = Command::new("wine");
        // 引数の日本語を wine が正しく読めるように、UTF-8 のロケールにする。
        c.arg(&exe).env("WINEDEBUG", "-all").env("LANG", "C.UTF-8");
        c
    } else {
        Command::new(&exe)
    };
    let mut child = run
        .args(&args)
        .current_dir(&work)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&input).unwrap();
    let out = child.wait_with_output().unwrap();
    Ok(Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

fn check_stdout(case: &Path, run: &Run) -> Result<(), String> {
    if let Ok(want) = std::fs::read_to_string(case.with_extension("out")) {
        if run.stdout != want {
            return Err(format!(
                "標準出力が違います\n--- 期待 ---\n{}--- 実際 ---\n{}",
                want, run.stdout
            ));
        }
    }
    Ok(())
}

fn check(case: &Path, dir: &Path) -> Result<(), String> {
    let src = std::fs::read_to_string(case).unwrap();
    // ほかの OS 向けのテストは飛ばす。
    let target = if cfg!(windows) || on_wine() {
        "windows"
    } else {
        "linux"
    };
    if header(&src, "os:").is_some_and(|os| os != target) {
        return Ok(());
    }
    if let Some(exit) = header(&src, "exit:") {
        let want: i32 = exit.parse().unwrap();
        for no_fold in [false, true] {
            let run =
                build_and_run(case, dir, no_fold, &src).map_err(|e| format!("コンパイルエラー:\n{}", e))?;
            if run.code != want {
                return Err(format!(
                    "終了コード {}（期待 {}、no_fold={}）\n{}",
                    run.code, want, no_fold, run.stderr
                ));
            }
            check_stdout(case, &run)?;
        }
    } else if let Some(msg) = header(&src, "error:") {
        match build_and_run(case, dir, false, &src) {
            Ok(_) => return Err("コンパイルエラーになりませんでした".into()),
            Err(e) if e.contains(&msg) => {}
            Err(e) => return Err(format!("エラーの内容が違います:\n{}", e)),
        }
    } else if let Some(msg) = header(&src, "runtime:") {
        let no_fold = header(&src, "flags:").is_some_and(|f| f.contains("--no-fold"));
        let run = build_and_run(case, dir, no_fold, &src).map_err(|e| format!("コンパイルエラー:\n{}", e))?;
        if run.code != PANIC_EXIT_CODE
            || !run.stderr.contains(&msg)
            || !run.stderr.starts_with("実行時エラー: ")
        {
            return Err(format!(
                "終了コード {}、標準エラー出力:\n{}",
                run.code, run.stderr
            ));
        }
        check_stdout(case, &run)?;
    } else {
        return Err("先頭に // exit: か // error: か // runtime: がありません".into());
    }
    Ok(())
}

#[test]
fn cases() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/cases");
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cases");
    std::fs::create_dir_all(&dir).unwrap();
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "orz"))
        .collect();
    cases.sort();
    let mut failed = Vec::new();
    for case in &cases {
        if let Err(e) = check(case, &dir) {
            failed.push(format!(
                "--- {} ---\n{}",
                case.file_name().unwrap().to_string_lossy(),
                e
            ));
        }
    }
    assert!(
        failed.is_empty(),
        "{} / {} 件が失敗:\n{}",
        failed.len(),
        cases.len(),
        failed.join("\n")
    );
    eprintln!("{} 件すべて成功", cases.len());
}
