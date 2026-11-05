//! tests/cases/*.orz をすべてコンパイルして試す。
//!
//! ファイルの先頭のコメントで、期待する結果を書く。
//!   // exit: 3          正しく動いて、終了コードが 3。定数の畳み込みあり・なしの両方で試す
//!   // error: 文字列     コンパイルエラーになり、その文字列が表示される
//!   // runtime: 文字列   定数の畳み込みを切ると実行時エラーになり、その文字列が表示される

use std::path::{Path, PathBuf};
use std::process::Command;

const PANIC_EXIT_CODE: i32 = 101;

fn orz() -> &'static str {
    env!("CARGO_BIN_EXE_orz")
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

/// 実行ファイルを作って実行する。（終了コード, 標準エラー出力）
fn build_and_run(case: &Path, dir: &Path, no_fold: bool) -> Result<(i32, String), String> {
    let exe = dir.join(format!(
        "{}{}",
        case.file_stem().unwrap().to_str().unwrap(),
        if no_fold { "-nofold" } else { "" }
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
    let out = cmd.output().unwrap();
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    let run = Command::new(&exe).output().unwrap();
    Ok((
        run.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&run.stderr).into_owned(),
    ))
}

fn check(case: &Path, dir: &Path) -> Result<(), String> {
    let src = std::fs::read_to_string(case).unwrap();
    if let Some(exit) = header(&src, "exit:") {
        let want: i32 = exit.parse().unwrap();
        for no_fold in [false, true] {
            let (code, err) =
                build_and_run(case, dir, no_fold).map_err(|e| format!("コンパイルエラー:\n{}", e))?;
            if code != want {
                return Err(format!(
                    "終了コード {}（期待 {}、no_fold={}）\n{}",
                    code, want, no_fold, err
                ));
            }
        }
    } else if let Some(msg) = header(&src, "error:") {
        match build_and_run(case, dir, false) {
            Ok(_) => return Err("コンパイルエラーになりませんでした".into()),
            Err(e) if e.contains(&msg) => {}
            Err(e) => return Err(format!("エラーの内容が違います:\n{}", e)),
        }
    } else if let Some(msg) = header(&src, "runtime:") {
        let (code, err) = build_and_run(case, dir, true).map_err(|e| format!("コンパイルエラー:\n{}", e))?;
        if code != PANIC_EXIT_CODE || !err.contains(&msg) || !err.starts_with("実行時エラー: ") {
            return Err(format!("終了コード {}、標準エラー出力:\n{}", code, err));
        }
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
