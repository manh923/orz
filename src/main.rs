//! orz コンパイラ。
//!
//! 使い方：
//!   orz build ファイル.orz [-o 出力]   実行ファイルを作る
//!   orz run ファイル.orz               作って、すぐ実行する
//!   orz asm ファイル.orz [-o 出力.s]   アセンブリだけ出す（`-o -` なら画面に）

mod ast;
mod fold;
mod ir;
mod lexer;
mod parser;
mod sema;
mod source;
mod x86;

use std::path::{Path, PathBuf};
use std::process::{exit, Command};

use source::SourceFile;

const USAGE: &str = "使い方:
  orz build ファイル.orz [-o 出力]   実行ファイルを作る
  orz run ファイル.orz               作って、すぐ実行する
  orz asm ファイル.orz [-o 出力.s]   アセンブリだけ出す（`-o -` なら画面に）";

struct Options {
    cmd: String,
    input: PathBuf,
    output: Option<PathBuf>,
    /// 開発用：定数の畳み込みをしない（実行時の検査を試すため）。
    no_fold: bool,
}

fn parse_args() -> Options {
    let mut args = std::env::args().skip(1);
    let Some(cmd) = args.next() else { usage_exit() };
    if cmd == "-h" || cmd == "--help" || cmd == "help" {
        println!("{}", USAGE);
        exit(0);
    }
    if !matches!(cmd.as_str(), "build" | "run" | "asm") {
        eprintln!("`{}` というコマンドはありません\n{}", cmd, USAGE);
        exit(2);
    }
    let mut input = None;
    let mut output = None;
    let mut no_fold = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "-o" => match args.next() {
                Some(o) => output = Some(PathBuf::from(o)),
                None => {
                    eprintln!("`-o` の後に出力ファイルの名前が必要です");
                    exit(2);
                }
            },
            "--no-fold" => no_fold = true,
            _ if input.is_none() => input = Some(PathBuf::from(a)),
            _ => {
                eprintln!("引数 `{}` の意味がわかりません\n{}", a, USAGE);
                exit(2);
            }
        }
    }
    let Some(input) = input else { usage_exit() };
    Options {
        cmd,
        input,
        output,
        no_fold,
    }
}

fn usage_exit() -> ! {
    eprintln!("{}", USAGE);
    exit(2);
}

/// ソースからアセンブリを作る。エラーなら表示して終わる。
fn compile(path: &Path, no_fold: bool) -> String {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{} を読めません: {}", path.display(), e);
            exit(1);
        }
    };
    let src = SourceFile::new(path.display().to_string(), text);
    let result = lexer::lex(&src.text)
        .and_then(|l| parser::parse(l.tokens))
        .and_then(|p| sema::lower(&p))
        .and_then(|mut ir| {
            if !no_fold {
                fold::fold(&mut ir)?;
            }
            Ok(ir)
        });
    match result {
        Ok(ir) => x86::emit(&ir, &src),
        Err(d) => {
            eprint!("{}", d.render(&src));
            exit(1);
        }
    }
}

/// アセンブルしてリンクする。
fn assemble_and_link(asm: &str, out: &Path) {
    let dir = std::env::temp_dir().join(format!("orz-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("作業用フォルダを作れません: {}", e);
        exit(1);
    }
    let s = dir.join("out.s");
    let o = dir.join("out.o");
    std::fs::write(&s, asm).expect("アセンブリを書けません");
    run_tool(Command::new("as").arg("-o").arg(&o).arg(&s), "as");
    run_tool(Command::new("ld").arg("-o").arg(out).arg(&o), "ld");
    let _ = std::fs::remove_dir_all(&dir);
}

fn run_tool(cmd: &mut Command, name: &str) {
    match cmd.status() {
        Ok(st) if st.success() => {}
        Ok(st) => {
            eprintln!("{} が失敗しました（{}）", name, st);
            exit(1);
        }
        Err(e) => {
            eprintln!("{} を実行できません: {}（binutils が必要です）", name, e);
            exit(1);
        }
    }
}

fn main() {
    let opt = parse_args();
    let asm = compile(&opt.input, opt.no_fold);
    match opt.cmd.as_str() {
        "asm" => {
            let out = opt.output.unwrap_or_else(|| opt.input.with_extension("s"));
            if out.as_os_str() == "-" {
                print!("{}", asm);
                return;
            }
            if let Err(e) = std::fs::write(&out, asm) {
                eprintln!("{} に書けません: {}", out.display(), e);
                exit(1);
            }
        }
        "build" => {
            let mut out = opt.output.unwrap_or_else(|| opt.input.with_extension(""));
            if out == opt.input {
                // 拡張子がないファイルだと、元のファイルを上書きしてしまう。
                out = opt.input.with_extension("out");
            }
            assemble_and_link(&asm, &out);
        }
        "run" => {
            let out = std::env::temp_dir().join(format!("orz-run-{}", std::process::id()));
            assemble_and_link(&asm, &out);
            let status = Command::new(&out).status();
            let _ = std::fs::remove_file(&out);
            match status {
                Ok(st) => exit(st.code().unwrap_or(1)),
                Err(e) => {
                    eprintln!("実行できません: {}", e);
                    exit(1);
                }
            }
        }
        _ => unreachable!(),
    }
}
