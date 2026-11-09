# orz（土下座）for VS Code

orz 言語（`.orz`、`.orh`）の色分けと、括弧・コメントの補助です。

## 入れ方

1. VS Code で `Ctrl+Shift+P` を押し、「Extensions: Install from VSIX...」（日本語の表示なら「VSIX からのインストール」）を選ぶ
2. このフォルダの `orz-0.1.2.vsix` を選ぶ

コマンドで入れるなら次のとおりです。

```sh
code --install-extension editors/vscode/orz-0.1.2.vsix
```

## 作り直し方

色分けの決まりは `syntaxes/orz.tmLanguage.json` にあります。書き換えたら、次のコマンドで `.vsix` を作り直します（Python だけで作れます）。

```sh
python3 editors/vscode/package.py
```
