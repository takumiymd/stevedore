```text
 ███████╗████████╗███████╗██╗   ██╗███████╗██████╗  ██████╗ ██████╗ ███████╗
 ██╔════╝╚══██╔══╝██╔════╝██║   ██║██╔════╝██╔══██╗██╔═══██╗██╔══██╗██╔════╝
 ███████╗   ██║   █████╗  ██║   ██║█████╗  ██║  ██║██║   ██║██████╔╝█████╗  
 ╚════██║   ██║   ██╔══╝  ╚██╗ ██╔╝██╔══╝  ██║  ██║██║   ██║██╔══██╗██╔══╝  
 ███████║   ██║   ███████╗ ╚████╔╝ ███████╗██████╔╝╚██████╔╝██║  ██║███████╗
 ╚══════╝   ╚═╝   ╚══════╝  ╚═══╝  ╚══════╝╚═════╝  ╚══════╝╚═╝  ╚═╝╚══════╝
```

# Stevedore

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2021-blue.svg)](https://www.rust-lang.org/)
[![Built With Ratatui](https://ratatui.rs/built-with-ratatui/badge.svg)](https://ratatui.rs/)

[English](README.md) | 日本語

Docker コンテナと Docker Compose スタックを管理するための、高速でキーボード駆動なターミナル UI（TUI）アプリケーションです。

## Looks

![Stevedore UI Demo](assets/stevedore.gif)


## Features

- 実行中 / 停止中のインジケータ付きでコンテナリストをリアルタイム表示（2秒ごとに自動更新）
- 詳細ペイン：イメージ、状態、ステータス、ポート、Compose プロジェクト / サービス名、および CPU・メモリ・ネットワークのリアルタイム統計を表示
- スクロールバック可能なリアルタイムのログストリーミング
- キーボード操作だけでコンテナの起動、停止、再起動が可能
- ワンキーでの Compose サービス更新機能：最新イメージのプルからサービスの再作成（`docker compose pull` + `up -d --build --no-deps`）までを自動化し、コマンドの出力結果を UI 上にストリーミング表示

## Requirements

- Rust ツールチェーン (edition 2021)
- ローカルのデフォルトソケット経由でアクセス可能な Docker デーモン
- Compose プラグインがインストールされた `docker` CLI（更新機能を使用する場合のみ必要）

## Installation

### Via Cargo
crates.io から直接インストールできます：

```sh
cargo install stevedore-tui
```

### From Source
リポジトリをクローンしてローカルでビルドすることも可能です：

```sh
git clone https://github.com/takumiymd/stevedore.git
cd stevedore
cargo install --path .
```

これによりリリースモードでバイナリがコンパイルされ、Cargo のバイナリディレクトリ（通常は `~/.cargo/bin/`）にインストールされます。以降、ターミナルから `stevedore` コマンドがグローバルに利用できるようになります。

## Build and run

```sh
cargo run
```

リリースビルドの場合：

```sh
cargo build --release
./target/release/stevedore
```

## Keybindings

| キー | アクション |
| --- | --- |
| 上下矢印 または k/j | コンテナリストのナビゲーション |
| Enter | 詳細ビューとリアルタイムログビューの切り替え |
| s | 選択したコンテナの起動 / 停止 |
| r | 選択したコンテナの再起動 |
| u | 選択した Compose サービスの更新 |
| PgUp / PgDn | ログのスクロールバック、または更新出力のスクロール |
| Home / End | 一番上へジャンプ / フォローモード（最新行の追従）へ戻る |
| Esc | 詳細ビューへ戻る |
| q または Ctrl+C | 終了 |

現在のビューで適用可能なキーバインドは、常に画面下部のフッターに表示されます。

## Architecture

非同期アクターに副作用を分離した MVU (Model-View-Update) アーキテクチャを採用しています：

- `src/app.rs`: モデル (`App`) と更新関数。すべての状態遷移はここで処理されます。
- `src/ui.rs`: ratatui を使用したモデルのピュアなレンダリング。
- `src/docker.rs`: bollard クライアントを所有するバックグラウンドアクター。コンテナ状態のポーリング、ログのストリーミング、アクションや Compose 更新の実行を行い、結果をメッセージとして返します。
- `src/main.rs`: ターミナルのセットアップと、入力イベントとアクターのメッセージを多重化する `tokio::select!` イベントループ。

UI スレッドは Docker API 呼び出しによってブロックされることはありません。すべてのアクションは個別のタスクとして実行され、結果はメッセージとして到着します。

## Roadmap

- Compose スタックの全サービスの一括更新機能
- コンテナのフィルタリングと検索機能
- Compose プロジェクトごとのコンテナリストのグループ化表示

## License

本プロジェクトは MIT ライセンスの下で公開されています。詳細については [LICENSE](LICENSE) ファイルを参照してください。
