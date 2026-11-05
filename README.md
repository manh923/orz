# orz

かんたんなげんごつくうど

orz（日本語名：土下座）は、C の書きやすさと安全確認を組み合わせた、小さな静的型付き言語です。仕様は [SPEC.md](SPEC.md) にあります。

```
function 32 main() {
    return 1 + 2;
}
```

## 今できること

実装の手順 1〜4 まで。

- 型は `32` と `bool`
- 変数（ブロック単位のスコープ）、`if`・`else if`・`else`、`while`・`break`・`continue`
- 関数（再帰、定義より前からの呼び出し、7 個以上の引数）
- グローバル変数と `immutable`
- `+ - * / % ^`、`not`・符号の `-`、`and nand or nor xor xnor`（`bool` なら短絡評価）、`<< >>`、比較
- 値がコンパイル時に決まる式のオーバーフロー・0 除算などは、コンパイルエラー
- それ以外は実行時に検出して止まる（`実行時エラー: ファイル名:行: 理由`、終了コード 101）。再帰が深すぎてスタックが溢れたときも同じ
- `main` の戻り値が終了コードになる

```
function 32 fib(n:32) {
    if (n < 2) {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}

function 32 main() {
    return fib(10);
}
```

文字列・配列・`print` などは、構文としては読めますが「まだ実装していません」のエラーになります（手順 5 以降）。

## 使い方

Rust と、x86-64 の Linux（binutils の `as` と `ld`）が必要です。

```sh
cargo build --release
./target/release/orz run three.orz              # 作って、すぐ実行する
./target/release/orz build three.orz -o three   # 実行ファイルを作る
./target/release/orz asm three.orz -o -         # アセンブリを画面に出す
```

## テスト

```sh
cargo test
```

`tests/cases/*.orz` をすべてコンパイルして試します。ファイルの先頭のコメントに、期待する結果を書きます。

| 書き方 | 意味 |
| --- | --- |
| `// exit: 3` | 正しく動いて、終了コードが 3。定数の畳み込みあり・なしの両方で試す |
| `// error: 文字列` | コンパイルエラーになり、その文字列が表示される |
| `// runtime: 文字列` | 実行時エラーになり、その文字列が表示される |
| `// flags: --no-fold` | 定数の畳み込みを切ってコンパイルする（数だけの式で実行時の検査を試すため） |

## 中身

| ファイル | 役目 |
| --- | --- |
| `src/lexer.rs` | 字句解析 |
| `src/parser.rs` | 構文解析 |
| `src/sema.rs` | 型の確認と、中間表現（三番地コード）への変換 |
| `src/fold.rs` | 定数の畳み込み。コンパイル時にわかる誤りはここで出す |
| `src/x86.rs` | x86-64 のアセンブリを出す。C ライブラリは使わない |
