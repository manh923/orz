# orz

かんたんなげんごつくうど

orz（日本語名：土下座）は、C の書きやすさと安全確認を組み合わせた、小さな静的型付き言語です。仕様は [SPEC.md](SPEC.md) にあります。

```
function 32 main() {
    return 1 + 2;
}
```

## 今できること

実装の手順 1〜5 まで。

```
import stdio;

function maybe:string find(names:array:string, target:string) {
    i:32 = 0;
    while (i < length(names)) {
        if (names[i] == target) {
            return names[i];
        }
        i = i + 1;
    }
    return null;
}

function nothing main() {
    names:array:string = ["apple", "banana"];
    found:maybe:string = find(names, "banana");
    if (found != null) {
        stdio.print("見つかった: " + found);
    }
    stdio.print(names);
}
```

- 型：`8`・`32`・`64`、`bool`、`string`、`array`、`pointer`、`address`、`maybe`（string・array・pointer に付けたもの）
- 変数（ブロック単位のスコープ）、`if`・`else if`・`else`、`while`・`break`・`continue`
- 関数（再帰、定義より前からの呼び出し、7 個以上の引数）、`main(args:array:string)`
- グローバル変数と `immutable`
- 文字列の `+`・`==`、配列（範囲チェックあり）、`allocate`・`size`・`bytes`・`to:型(x)`
- `import stdio;` の `print`・`put`・`read_line`・`read_file`・`write_file`・`last_error`
- `if (x != null)` などで、`maybe` を外して使える（絞り込み）
- 値がコンパイル時に決まる式のオーバーフロー・0 除算などは、コンパイルエラー
- それ以外は実行時に検出して止まる（`実行時エラー: ファイル名:行: 理由`、終了コード 101）。配列の範囲外、変換の範囲外、スタックの溢れなども同じ

構造体・`decnum`・自分のファイルの `import` などは、構文としては読めますが「まだ実装していません」のエラーになります（手順 6 以降）。

## 使い方

x86-64 の Linux で動きます。作るときには Rust と C コンパイラ（ランタイム用）が、使うときには binutils（`as` と `ld`）が必要です。

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
| `// args: a b c` | 実行するときのコマンドライン引数 |

同じ名前の `.out` があれば標準出力がその中身と同じかを確かめ、`.in` があれば標準入力に流します。

## 中身

| ファイル | 役目 |
| --- | --- |
| `src/lexer.rs` | 字句解析 |
| `src/parser.rs` | 構文解析 |
| `src/sema.rs` | 型の確認と、中間表現（三番地コード）への変換 |
| `src/fold.rs` | 定数の畳み込み。コンパイル時にわかる誤りはここで出す |
| `src/x86.rs` | x86-64 のアセンブリを出す |
| `runtime/orz_runtime.c` | ランタイム（表示、文字列、メモリ、ファイルなど）。C ライブラリは使わず、システムコールだけ |
| `build.rs` | ランタイムをコンパイルして、コンパイラに埋め込む |
