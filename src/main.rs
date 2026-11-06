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

/// ランタイム（runtime/orz_runtime.c）をコンパイルしたもの。build.rs が作る。
const RUNTIME_OBJ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/orz_runtime.o"));

const USAGE: &str = "使い方:
  orz build ファイル.orz [-o 出力]   実行ファイルを作る
  orz run ファイル.orz               作って、すぐ実行する
  orz asm ファイル.orz [-o 出力.s]   アセンブリだけ出す（`-o -` なら画面に）

オプション:
  --no-gc   GC を動かさず、終了時にまとめて解放する（すぐ終わるプログラム向け）";

struct Options {
    cmd: String,
    input: PathBuf,
    output: Option<PathBuf>,
    /// 開発用：定数の畳み込みをしない（実行時の検査を試すため）。
    no_fold: bool,
    /// GC を動かさない（終了時にまとめて解放する）。すぐ終わるプログラム向け。
    no_gc: bool,
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
    let mut no_gc = false;
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
            "--no-gc" => no_gc = true,
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
        no_gc,
    }
}

fn usage_exit() -> ! {
    eprintln!("{}", USAGE);
    exit(2);
}

/// ソースからアセンブリを作る。エラーなら表示して終わる。
fn compile(path: &Path, no_fold: bool, no_gc: bool) -> String {
    let mut files = Files {
        srcs: Vec::new(),
        paths: Vec::new(),
        progs: Vec::new(),
    };
    let result = files
        .load_all(path)
        .and_then(|_| sema::lower(&files.progs))
        .and_then(|mut ir| {
            if !no_fold {
                fold::fold(&mut ir)?;
            }
            Ok(ir)
        });
    match result {
        Ok(ir) => x86::emit(&ir, &files.srcs, no_gc),
        Err(d) => {
            let src = files.srcs.get(d.span.file as usize).unwrap_or(&files.srcs[0]);
            eprint!("{}", d.render(src));
            exit(1);
        }
    }
}

/// 読み込んだファイル。番号は `Span::file` と同じ。0 番が最初に渡されたファイル。
struct Files {
    srcs: Vec<SourceFile>,
    /// 同じファイルを 2 回読まないための、正規化したパス。
    paths: Vec<PathBuf>,
    progs: Vec<ast::Program>,
}

impl Files {
    /// 最初のファイルと、そこから import されるファイルをすべて読む。
    fn load_all(&mut self, root: &Path) -> Result<(), source::Diag> {
        let text = match std::fs::read_to_string(root) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("{} を読めません: {}", root.display(), e);
                exit(1);
            }
        };
        let canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        self.add(root.display().to_string(), canon, text)?;
        let mut i = 0;
        while i < self.progs.len() {
            let dir = self.paths[i].parent().map(Path::to_path_buf).unwrap_or_default();
            // 読む順に関係なく、import に読み込んだファイルの番号を書き込む。
            let mut resolved = Vec::new();
            for (k, item) in self.progs[i].items.iter().enumerate() {
                let ast::Item::Import(imp) = item else { continue };
                let ast::ImportTarget::File(rel) = &imp.target else {
                    continue;
                };
                let full = dir.join(rel);
                let canon = full.canonicalize().map_err(|_| {
                    source::Diag::new(imp.span, format!("ファイル `{}` が見つかりません", rel))
                        .hint("パスは、import を書いたファイルから見た場所で書きます")
                })?;
                resolved.push((k, canon, imp.span, self.display_name(i, rel)));
            }
            for (k, canon, span, name) in resolved {
                let index = match self.paths.iter().position(|p| *p == canon) {
                    Some(j) => j,
                    None => {
                        let text = std::fs::read_to_string(&canon).map_err(|e| {
                            source::Diag::new(span, format!("ファイル `{}` を読めません: {}", name, e))
                        })?;
                        self.add(name, canon, text)?
                    }
                };
                if let ast::Item::Import(imp) = &mut self.progs[i].items[k] {
                    imp.resolved = Some(index);
                }
            }
            i += 1;
        }
        Ok(())
    }

    /// エラーの表示に使う名前（最初のファイルのあるフォルダから見たパス）。
    fn display_name(&self, from: usize, rel: &str) -> String {
        let base = Path::new(&self.srcs[from].name)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        base.join(rel).display().to_string()
    }

    fn add(&mut self, name: String, canon: PathBuf, text: String) -> Result<usize, source::Diag> {
        let index = self.srcs.len();
        self.srcs.push(SourceFile::new(name, text));
        self.paths.push(canon);
        let lexed = lexer::lex(&self.srcs[index].text, index as u32)?;
        self.progs.push(parser::parse(lexed.tokens)?);
        Ok(index)
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
    let rt = dir.join("orz_runtime.o");
    std::fs::write(&s, asm).expect("アセンブリを書けません");
    std::fs::write(&rt, RUNTIME_OBJ).expect("ランタイムを書けません");
    run_tool(Command::new("as").arg("-o").arg(&o).arg(&s), "as");
    run_tool(Command::new("ld").arg("-o").arg(out).arg(&o).arg(&rt), "ld");
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
    let asm = compile(&opt.input, opt.no_fold, opt.no_gc);
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
