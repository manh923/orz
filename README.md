# orz

かんたんなげんごつくうど

orz（日本語名：土下座）は、C の書きやすさと安全確認を組み合わせた、小さな静的型付き言語です。仕様は [SPEC.md](SPEC.md) にあります。

```
function 32 main() {
    return 1 + 2;
}
```

## 今できること

実装の手順 1〜8 まで（全部）。

```
import stdio;

struct Node {
    value:32;
    next:maybe:Node;
}

function 32 sum(list:maybe:Node) {
    s:32 = 0;
    cur:maybe:Node = list;
    while (cur != null) {
        s = s + cur.value;
        cur = cur.next;
    }
    return s;
}

function nothing main() {
    list:Node = Node(1, Node(2, Node(3, null)));
    stdio.print(sum(list));          // 6
    stdio.print(list);               // Node(value: 1, next: Node(value: 2, …))
    stdio.print(0.1 + 0.2);          // 0.30000000000000004
    n:maybe:32 = to:32("42");
    if (n != null) {
        stdio.print(n + 1);          // 43
    }
}
```

- 型：`8`・`32`・`64`、`bool`、`decnum`、`string`、`array`、`pointer`、`address`、構造体、`maybe`
- 変数（ブロック単位のスコープ）、`if`・`else if`・`else`、`while`・`break`・`continue`
- 関数（再帰、定義より前からの呼び出し、7 個以上の引数）、`main(args:array:string)`
- グローバル変数と `immutable`
- 構造体（`Point(1, 2)`、`p.x`、`copy`）、文字列の `+`・`==`、配列（範囲チェックあり）、`allocate`・`size`・`bytes`・`to:型(x)`
- `import stdio;` の `print`・`put`・`read_line`・`read_file`・`write_file`・`last_error`
- `import bignum;`：大きな整数（`bignum.from`・`parse`・`add`・`sub`・`mul`・`div`・`mod`・`pow`・`compare`・`print` など）。orz で書いてある（[lib/bignum.orz](lib/bignum.orz)）
- `import strings;`・`lists;`・`maps;`・`os;`：文字列（切り出し・検索・分割・`Builder`）、長さの変わる配列、文字列をキーにした表、コマンドの実行・環境変数など。どれも orz で書いてある（[lib/](lib/)）
- `import math;`：`sqrt`・`sin`・`log` など（誤差は最後の桁で 1 未満）。`import random;`：数値計算・ゲーム用の乱数。`import crypto;`：暗号用の、OS の安全な乱数
- `import socket;`：TCP の通信。Linux はシステムコール、Windows は ws2_32.dll を、orz から直接呼んでいる
- `import http;`：HTTP のサーバーとクライアント。`http.serve("127.0.0.1", 8080, answer)` で、リクエストのたびに関数 `answer` が呼ばれる
- `import sys;`：OS への入口。`sys.call(番号, …)` で Linux のシステムコール、`extern "kernel32.dll" function …;` で Windows の DLL の関数を呼べる。OS によって違うところは `名前_linux.orz`・`名前_windows.orz` に分けて書く
- `extern function 型 名前(…);` と `.orh`：ランタイムや C の関数を呼ぶ（`--link` でオブジェクトを足せる）
- `array(n, 初期値)`、`stdio.eprint`・`eput`（標準エラー出力）
- 自分のファイルの `import "util.orz";` と `export`
- 関数ポインタ（`f:function:32(32, 32) = add;`）と、ジェネリクス（`struct List:T`、`function T get:T(…)`。型は呼び出しから推論）
- GC（保守的なマーク・アンド・スイープ）。`--no-gc` で切れる
- `arena { }`：中で作ったものを、閉じ `}` でまとめて解放する。中で作った値を外に持ち出すと、呼んだ関数の中での代入も含めてコンパイルエラー
- `if (x != null)` などで、`maybe` を外して使える（絞り込み）
- 値がコンパイル時に決まる式のオーバーフロー・0 除算などは、コンパイルエラー
- それ以外は実行時に検出して止まる（`実行時エラー: ファイル名:行: 理由`、終了コード 101）。配列の範囲外、変換の範囲外、スタックの溢れなども同じ

- 整形ツール（`orz fmt`）：`;`・`{`・`}` で改行と字下げを決め、コメントと空行（1 行まで）を残す


```
import bignum;

function nothing main() {
    f:bignum.Big = bignum.from(1);
    i:64 = 1;
    while (i <= 50) {
        f = bignum.mul(f, bignum.from(i));
        i = i + 1;
    }
    bignum.print(f);   // 30414093201713378043612608166064768844377641568960512000000000000
}
```

## 使い方

x86-64 の Linux と Windows で動きます。作るときには Rust と C コンパイラ（ランタイム用）が、使うときには binutils（`as` と `ld`）が必要です。

```sh
cargo build --release
./target/release/orz run three.orz              # 作って、すぐ実行する
./target/release/orz build three.orz -o three   # 実行ファイルを作る
./target/release/orz asm three.orz -o -         # アセンブリを画面に出す
./target/release/orz fmt three.orz              # 整形して、ファイルを書き換える（`-o -` なら画面に）
```

### Windows

MinGW-w64 の gcc と binutils を使います。MSYS2 なら、MINGW64 のシェルで次のように入れます。

```sh
pacman -S mingw-w64-x86_64-gcc
```

C コンパイラが `cc` という名前で見つからなければ、`CC=gcc cargo build --release` のように指定します。`orz build three.orz` で `three.exe` ができます。

### ほかの OS 向けに作る

`--target linux` か `--target windows` で、ほかの OS 向けの実行ファイルを作れます。Linux から Windows 向けに作るには、orz を作るときに `x86_64-w64-mingw32-gcc` が、使うときに `x86_64-w64-mingw32-as` と `x86_64-w64-mingw32-ld` が要ります（Ubuntu なら `gcc-mingw-w64-x86-64`）。

### VS Code

色分けの拡張機能が [editors/vscode](editors/vscode) にあります。`Ctrl+Shift+P` →「Extensions: Install from VSIX...」で、`editors/vscode/orz-0.1.3.vsix` を選ぶと入ります。

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
| `// flags: --no-fold` | コンパイルのオプション。`--no-fold` は定数の畳み込みを切る（数だけの式で実行時の検査を試すため）。`--no-gc` なども書ける |
| `// args: a b c` | 実行するときのコマンドライン引数 |

同じ名前の `.out` があれば標準出力がその中身と同じかを確かめ、`.in` があれば標準入力に流します。

`ORZ_TEST_TARGET=windows cargo test` で、Windows 向けに作って wine で動かします（Linux の上で Windows 版を試すため）。

## 中身

| ファイル | 役目 |
| --- | --- |
| `src/lexer.rs` | 字句解析 |
| `src/parser.rs` | 構文解析 |
| `src/sema.rs` | 型の確認と、中間表現（三番地コード）への変換 |
| `src/escape.rs` | arena の持ち出しの検査（関数をまたいで調べる） |
| `src/fold.rs` | 定数の畳み込み。コンパイル時にわかる誤りはここで出す |
| `src/fmt.rs` | 整形ツール（`orz fmt`） |
| `src/x86.rs` | x86-64 のアセンブリを出す |
| `runtime/orz_runtime.c` | ランタイム（表示、文字列、メモリ、ファイルなど）。C ライブラリは使わない。OS とのやりとり（Linux はシステムコール、Windows は kernel32）は「OS」の節にまとめてある |
| `build.rs` | ランタイムを OS ごとにコンパイルして、コンパイラに埋め込む |
| `editors/vscode/` | VS Code の拡張機能（色分け） |
| `lib/*.orz` | orz で書いた標準ライブラリ。ファイルを置くと、`import 名前;` で使えるようになる（コンパイラに埋め込む）。`名前_linux.orz`・`名前_windows.orz` は OS 別 |
