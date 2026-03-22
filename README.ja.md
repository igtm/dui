# dui

`dui` は、日常のローカル開発向けに作った container-first の Docker TUI です。

[English README](./README.md)

images / networks / volumes ではなく、container と container logs を主役にした UI に寄せています。現時点のリリースでは、次を重視しています。

- Compose の project / service 文脈つきの高速な container list
- 単一 container の logs 表示、検索、substring / regex filter、follow、wrap、clipboard copy
- overview / env vars / ports / mounts / health の構造化 inspect 表示
- start/stop、restart、remove を keyboard-first で操作

## Install

GitHub から Cargo で install:

```bash
cargo install --git https://github.com/igtm/dui.git --locked
```

local checkout から install:

```bash
cargo install --path . --locked
```

## 実行

```bash
cargo run -- --all
```

起動時に filter や focus を渡すこともできます。

```bash
cargo run -- --project demo --container api --theme ember
```

binary を直接起動する場合:

```bash
cargo build
target/debug/dui --all
```

## キーバインド

- `q`: 終了
- `Tab`: container list と detail pane の focus 切り替え
- `1-6`: detail tab 切り替え
- `a`: stopped container の表示切り替え
- `y`: 現在の選択を copy
- `s`: 選択 container の start / stop
- `r`: restart
- `D`: 確認つき remove
- `/`: logs 検索
- `f`: logs filter
- `m`: substring / regex filter の切り替え
- マウスホイール: hover 中の selection を 1 行ずつ移動
- logs 上で左ドラッグ: 連続範囲選択
- 右側 scrollbar のドラッグ: 表示 window の移動
- `Space`: log follow の切り替え
- `w`: logs wrap の切り替え
- `t`: timestamp 表示の切り替え

## 設定

config の既定パス:

- Linux: `$XDG_CONFIG_HOME/dui/config.toml`
- macOS: `~/Library/Application Support/dui/config.toml`

設定例は [`examples/config.toml`](./examples/config.toml) を参照してください。

## ステータス

`dui` は Linux / macOS のローカル Docker Engine 利用を前提にしています。container と container logs に集中するためのツールで、images / networks / volumes はスコープ外です。
