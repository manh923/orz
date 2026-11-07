//! orz コンパイラ。
//!
//! 使い方：
//!   orz build ファイル.orz [-o 出力]   実行ファイルを作る
//!   orz run ファイル.orz               作って、すぐ実行する
//!   orz asm ファイル.orz [-o 出力.s]   アセンブリだけ出す（`-o -` なら画面に）
//!   orz fmt ファイル.orz [-o 出力]     整形する（`-o` がなければ、そのファイルを書き換える）

mod ast;
mod escape;
mod fmt;
mod fold;
mod ir;
mod lexer;
mod parser;
mod sema;
mod source;
mod x86;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

use source::SourceFile;
use x86::Os;

/// ランタイム（runtime/orz_runtime.c）をコンパイルしたもの。build.rs が作る。作れなかった OS の分は空。
const RUNTIME_LINUX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/orz_runtime_linux.o"));
const RUNTIME_WINDOWS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/orz_runtime_windows.o"));

/// orz で書いた標準ライブラリ（`import bignum;` などで読む）。
const STD_LIBS: &[(&str, &str)] = &[
    ("bignum", include_str!("../lib/bignum.orz")),
    ("strings", include_str!("../lib/strings.orz")),
    ("lists", include_str!("../lib/lists.orz")),
    ("maps", include_str!("../lib/maps.orz")),
    ("os", include_str!("../lib/os.orz")),
];

/// orz が動いている OS。
const HOST: Os = if cfg!(windows) { Os::Windows } else { Os::Linux };

const USAGE: &str = "使い方:
  orz build ファイル.orz [-o 出力]   実行ファイルを作る
  orz run ファイル.orz               作って、すぐ実行する
  orz asm ファイル.orz [-o 出力.s]   アセンブリだけ出す（`-o -` なら画面に）
  orz fmt ファイル.orz [-o 出力]     整形する（`-o` がなければ、そのファイルを書き換える。`-o -` なら画面に）

オプション:
  --no-gc              GC を動かさず、終了時にまとめて解放する（すぐ終わるプログラム向け）
  --target 名前        linux か windows。どの OS 向けに作るか（ふつうは、今の OS）
  --link ファイル      一緒にリンクするオブジェクトやライブラリ（extern の関数の中身。何度でも書ける）";

struct Options {
    cmd: String,
    input: PathBuf,
    output: Option<PathBuf>,
    /// 開発用：定数の畳み込みをしない（実行時の検査を試すため）。
    no_fold: bool,
    /// GC を動かさない（終了時にまとめて解放する）。すぐ終わるプログラム向け。
    no_gc: bool,
    /// どの OS 向けに作るか。
    target: Os,
    /// 一緒にリンクするファイル（extern の関数の中身）。
    link: Vec<PathBuf>,
}

fn parse_args() -> Options {
    let mut args = std::env::args().skip(1);
    let Some(cmd) = args.next() else { usage_exit() };
    if cmd == "-h" || cmd == "--help" || cmd == "help" {
        println!("{}", USAGE);
        exit(0);
    }
    if !matches!(cmd.as_str(), "build" | "run" | "asm" | "fmt") {
        eprintln!("`{}` というコマンドはありません\n{}", cmd, USAGE);
        exit(2);
    }
    let mut input = None;
    let mut output = None;
    let mut no_fold = false;
    let mut no_gc = false;
    let mut target = HOST;
    let mut link = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--link" => match args.next() {
                Some(f) => link.push(PathBuf::from(f)),
                None => {
                    eprintln!("`--link` の後にファイルの名前が必要です");
                    exit(2);
                }
            },
            "--target" => {
                target = match args.next().as_deref() {
                    Some("linux") => Os::Linux,
                    Some("windows") => Os::Windows,
                    _ => {
                        eprintln!("`--target` の後には linux か windows を書きます");
                        exit(2);
                    }
                }
            }
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
        target,
        link,
    }
}

fn usage_exit() -> ! {
    eprintln!("{}", USAGE);
    exit(2);
}

/// ソースからアセンブリを作る。エラーなら表示して終わる。
fn compile(path: &Path, no_fold: bool, no_gc: bool, os: Os) -> String {
    let mut files = Files {
        srcs: Vec::new(),
        paths: Vec::new(),
        progs: Vec::new(),
    };
    let result = files
        .load_all(path)
        .and_then(|_| sema::lower(&files.progs))
        .and_then(|ir| escape::check(&ir, &files.srcs).map(|_| ir))
        .and_then(|mut ir| {
            if !no_fold {
                fold::fold(&mut ir)?;
            }
            Ok(ir)
        });
    match result {
        Ok(ir) => x86::emit(&ir, &files.srcs, os, no_gc),
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
                match &imp.target {
                    ast::ImportTarget::File(rel) => {
                        let full = dir.join(rel);
                        let canon = full.canonicalize().map_err(|_| {
                            source::Diag::new(imp.span, format!("ファイル `{}` が見つかりません", rel))
                                .hint("パスは、import を書いたファイルから見た場所で書きます")
                        })?;
                        resolved.push((k, canon, imp.span, self.display_name(i, rel), None));
                    }
                    ast::ImportTarget::Std(name) => {
                        // orz で書いた標準ライブラリは、コンパイラに埋め込んである。
                        if let Some((_, text)) = STD_LIBS.iter().find(|(n, _)| n == name) {
                            let shown = format!("std/{}.orz", name);
                            resolved.push((k, PathBuf::from(&shown), imp.span, shown, Some(*text)));
                        }
                    }
                }
            }
            for (k, canon, span, name, builtin) in resolved {
                let index = match self.paths.iter().position(|p| *p == canon) {
                    Some(j) => j,
                    None => {
                        let text = match builtin {
                            Some(t) => t.to_string(),
                            None => std::fs::read_to_string(&canon).map_err(|e| {
                                source::Diag::new(span, format!("ファイル `{}` を読めません: {}", name, e))
                            })?,
                        };
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
        let mut prog = parser::parse(lexed.tokens)?;
        if self.srcs[index].name.ends_with(".orh") {
            header_items(&mut prog)?;
        }
        self.progs.push(prog);
        Ok(index)
    }
}

/// `.orh`（宣言だけを書くファイル）に書けるのは、`extern` の関数、構造体、`immutable` だけ。
/// 読み込むためのファイルなので、すべて `export` したものとみなす。
fn header_items(prog: &mut ast::Program) -> Result<(), source::Diag> {
    for item in &mut prog.items {
        let bad = match item {
            ast::Item::Func(f) if f.is_extern => {
                f.exported = true;
                None
            }
            ast::Item::Func(f) => Some(f.name_span),
            ast::Item::Struct(st) => {
                st.exported = true;
                None
            }
            ast::Item::Global(d) if d.immutable => {
                d.exported = true;
                None
            }
            ast::Item::Global(d) => Some(d.name_span),
            ast::Item::Import(imp) => Some(imp.span),
        };
        if let Some(span) = bad {
            return Err(source::Diag::new(
                span,
                "`.orh` に書けるのは、`extern` の関数、`struct`、`immutable` だけです",
            )
            .hint("中身のある関数や変数は `.orz` に書きます"));
        }
    }
    Ok(())
}

/// アセンブルしてリンクする。
fn assemble_and_link(asm: &str, out: &Path, os: Os, extra: &[PathBuf]) {
    let (runtime, cross_cc) = match os {
        Os::Linux => (RUNTIME_LINUX, "x86_64-linux-gnu-gcc"),
        Os::Windows => (RUNTIME_WINDOWS, "x86_64-w64-mingw32-gcc"),
    };
    if runtime.is_empty() {
        eprintln!(
            "{} 向けのランタイムがありません（orz を作るときに、{} 向けの C コンパイラ {} が必要です）",
            os_name(os),
            os_name(os),
            cross_cc
        );
        exit(1);
    }
    let dir = std::env::temp_dir().join(format!("orz-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("作業用フォルダを作れません: {}", e);
        exit(1);
    }
    let s = dir.join("out.s");
    let o = dir.join("out.o");
    let rt = dir.join("orz_runtime.o");
    std::fs::write(&s, asm).expect("アセンブリを書けません");
    std::fs::write(&rt, runtime).expect("ランタイムを書けません");
    // 今の OS と違う OS 向けなら、その OS 向けの binutils（x86_64-w64-mingw32-as など）を使う。
    let prefix = match (os, os == HOST) {
        (_, true) => "",
        (Os::Linux, false) => "x86_64-linux-gnu-",
        (Os::Windows, false) => "x86_64-w64-mingw32-",
    };
    let (as_, ld) = (format!("{}as", prefix), format!("{}ld", prefix));
    run_tool(Command::new(&as_).arg("-o").arg(&o).arg(&s), &as_);
    let mut link = Command::new(&ld);
    link.arg("-o").arg(out).arg(&o).args(extra).arg(&rt);
    if os == Os::Windows {
        // スタックは Linux と同じ 8MB にする（Windows のふつうは 1MB）。
        link.args(["-e", "_start", "--subsystem", "console", "--stack", "8388608"]);
        link.arg(kernel32());
    }
    run_tool(&mut link, &ld);
    let _ = std::fs::remove_dir_all(&dir);
}

fn os_name(os: Os) -> &'static str {
    match os {
        Os::Linux => "Linux",
        Os::Windows => "Windows",
    }
}

/// kernel32 の関数を使えるようにする。Windows の上では kernel32.dll をそのまま渡す
/// （MinGW のライブラリの場所によらない）。ほかの OS からは MinGW の libkernel32.a を使う。
fn kernel32() -> OsString {
    if cfg!(windows) {
        if let Some(root) = std::env::var_os("SystemRoot") {
            let dll = Path::new(&root).join("System32").join("kernel32.dll");
            if dll.exists() {
                return dll.into_os_string();
            }
        }
    }
    "-lkernel32".into()
}

/// Windows 向けなら、拡張子のない名前に .exe を付ける。
fn exe_name(path: PathBuf, os: Os) -> PathBuf {
    if os == Os::Windows && path.extension().is_none() {
        path.with_extension("exe")
    } else {
        path
    }
}

fn run_tool(cmd: &mut Command, name: &str) {
    match cmd.status() {
        Ok(st) if st.success() => {}
        Ok(st) => {
            eprintln!("{} が失敗しました（{}）", name, st);
            exit(1);
        }
        Err(e) => {
            let hint = if cfg!(windows) {
                "binutils が必要です。Windows では MSYS2 などで MinGW-w64 の binutils を入れます"
            } else {
                "binutils が必要です"
            };
            eprintln!("{} を実行できません: {}（{}）", name, e, hint);
            exit(1);
        }
    }
}

/// `orz fmt`：整形する。字句や構文の誤りがあれば、書き換えずに止まる。
fn format_file(input: &Path, output: Option<PathBuf>) {
    let text = match std::fs::read_to_string(input) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{} を読めません: {}", input.display(), e);
            exit(1);
        }
    };
    let formatted = match fmt::format(&text) {
        Ok(f) => f,
        Err(d) => {
            eprint!(
                "{}",
                d.render(&SourceFile::new(input.display().to_string(), text))
            );
            exit(1);
        }
    };
    let out = output.unwrap_or_else(|| input.to_path_buf());
    if out.as_os_str() == "-" {
        print!("{}", formatted);
        return;
    }
    if out == input && formatted == text {
        return;
    }
    if let Err(e) = std::fs::write(&out, formatted) {
        eprintln!("{} に書けません: {}", out.display(), e);
        exit(1);
    }
}

fn main() {
    let opt = parse_args();
    if opt.cmd == "fmt" {
        format_file(&opt.input, opt.output);
        return;
    }
    let asm = compile(&opt.input, opt.no_fold, opt.no_gc, opt.target);
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
            let mut out = exe_name(
                opt.output.unwrap_or_else(|| opt.input.with_extension("")),
                opt.target,
            );
            if out == opt.input {
                // 拡張子がないファイルだと、元のファイルを上書きしてしまう。
                out = opt.input.with_extension("out");
            }
            assemble_and_link(&asm, &out, opt.target, &opt.link);
        }
        "run" => {
            if opt.target != HOST {
                eprintln!(
                    "{} 向けの実行ファイルは、ここでは実行できません（build で作ってください）",
                    os_name(opt.target)
                );
                exit(1);
            }
            let out = exe_name(
                std::env::temp_dir().join(format!("orz-run-{}", std::process::id())),
                opt.target,
            );
            assemble_and_link(&asm, &out, opt.target, &opt.link);
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
