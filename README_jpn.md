<p align="center">
  <img src="images/EH3D_BANNER.svg" alt="EH3D-SERVER banner" width="100%">
</p>

# 🧭 EH3D-SERVER

<p align="center"><a href="README.md">🇺🇸 English</a> | <a href="README_spa.md">🇪🇸 Español</a> | <a href="README_fra.md">🇫🇷 Français</a> | <a href="README_ita.md">🇮🇹 Italiano</a> | <a href="README_deu.md">🇩🇪 Deutsch</a> | <a href="README_zho.md">🇨🇳 中文</a> | 🇯🇵 <b>日本語</b> | <a href="README_por.md">🇵🇹 Português</a></p>

### EH3D プログラムの中心：DRILLPRO、ROUTEPRO、今後のクライアントからジョブを受け取り、計画し、ノード経由で機械上で実行し、進行状況をすべてに知らせます

<p align="center">
  <img src="https://img.shields.io/badge/License-GPL%203.0--or--later-blue.svg" alt="GPL 3.0 or later">
  <img src="https://img.shields.io/badge/Stack-Rust-DEA584.svg" alt="Rust">
  <img src="https://img.shields.io/badge/Languages-8-00E5FF.svg" alt="Languages">
  <img src="https://img.shields.io/badge/Version-0.0.1-0f6cbd.svg" alt="Version">
  <img src="https://img.shields.io/badge/Maturity-scaffolding-FFB020.svg" alt="Maturity">
</p>

---

**正直さの確認 - 今日実際に動くもの：** REST と WebSocket のインターフェースを持つ Rust サーバー（axum）で、ユーザー、機械、ジョブを保持します。EH3D-DRILLPRO と EH3D-ROUTEPRO のライブラリでドリルまたはルーティングのジョブ（穴とパス、または工具テーブル付きの Excellon／HP-GL ファイル）を計画し、プレビューを表示し、EH3D-NODE-BUNGARD 経由またはシミュレーターで機械上で実行します。オペレーターのダイアログ、停止、すべての変更のリアルタイム通知を備えます。39 件のテストがコンピューター上で動き、ジョブはシミュレーターに対して実行されます。**これを通して実機で動かしたことはまだなく**、TLS は終端せず、プログラム以外のクライアント（Android アプリ、Studio、機械の横のパネル）もまだ存在しません。

---

> **安全性とセキュリティの現状：** これは安全装置ではありません。ジョブは自動では開始されません。オペレーター権限の人が、1 台の機械で 1 度に 1 つだけ開始し、計画は、テーブル外の穴やパス、不足または範囲外の工具を拒否します。すべてのリクエストにトークンが必要です（閲覧者は読み取り、オペレーターはジョブを実行、管理者はユーザー・トークン・機械を管理）。パスワードは Argon2 ハッシュ、セッションとプログラムのトークンは SHA-256 のみで保存し、ノードのトークンは API が表示することはありません。サーバーは家庭内または VPN 向けです。外部から到達する前に HTTPS を前段に置いてください。機械の停止はサーバーだけに依存せず、リンクが途絶えるとノード自身が停止します。このプロジェクトは独立しており、Bungard Elektronik による製造・承認・サポートではありません。

## 📋 ひと目で

| | |
|---|---|
| バージョン | `0.0.1` |
| 成熟度 | 足場 |
| テスト | 39 |
| 技術スタック | Rust (axum, tokio) |
| 実機での動作 | いいえ：シミュレーターでテスト済み。サーバー経由で実機を動かしたことはまだありません |

**移動先：** [概要](#-概要) · [アーキテクチャ](#-アーキテクチャ) · [クイックスタート](#-クイックスタート) · [テスト](#-テスト) · [構成](#-リポジトリの構成) · [関連](#-関連プロジェクト)

## 🚦 プロジェクトの状況

各行はこのリポジトリの一部分で、信号は今日どこまで進んでいるかを示します。計画ではなく、リポジトリが示せるもの（テスト、ビルド、実行）に基づいて書かれています。

🟢 完了・検証済み · 🟡 動作する（既知の制限あり） · 🟠 一部のみ、または未検証 · 🔴 未実装または動作しない

| 領域 | 状態 | ここでの意味 |
|---|---|---|
| ソフトウェア | 🟡 | Rust サーバー（axum）がユーザー、機械、ジョブを保持し、EH3D-DRILLPRO と EH3D-ROUTEPRO のライブラリでドリル／ルーティングのジョブを計画し、プレビューを表示して、オペレーターのダイアログと停止付きで実行します。 |
| テスト | 🟢 | 39 件のテストが通ります（`cargo test`）：モジュール内 28 件、シミュレーターでジョブを動かすインターフェースのエンドツーエンド 11 件（`tests/api.rs` に 7 件、実プログラムを使う `tests/programs.rs` に 4 件）。 |
| クライアント向けインターフェース（API） | 🟡 | REST リクエストと WebSocket イベントは `docs/API.md` に記載され、テストで網羅されています。現時点で使っているのはプログラム自身のサーバー接続だけです。 |
| クライアント | 🔴 | Android アプリ、Studio、機械の横のタッチ パネルはまだ存在しません。 |
| セキュリティ | 🟡 | すべてのリクエストに 3 つの権限のいずれかを持つトークンが必要で、パスワードは Argon2 ハッシュ、誤ったパスワードは遅延されます。TLS は終端しない（前段に HTTPS を置く）ため、独立したレビューも受けていません。 |
| 実機 | 🟠 | ジョブはシミュレーターに対してのみ実行されました。サーバー経由で実機を動かしたことはありません。 |
| 配備 | 🟠 | systemd ユニットと `docs/DEPLOY.md` が CM5 上のサービスを説明していますが、このリポジトリには配備済みの記録はありません。 |
| 連携 | 🟡 | EH3D-DRILLPRO・EH3D-ROUTEPRO と同じライブラリで計画します（フォルダーはこのフォルダーの隣に必要）。EH3D-NODE-BUNGARD のリンクとは通信しますが、到達したのはシミュレーター経由のみです。 |
| ドキュメント | 🟢 | 8 言語の README、API リファレンス、サービスとして動かす手引き。 |

## 🎯 概要

* **ジョブ**：ドリルのジョブ（穴と工具、または工具テーブル付きの `.NCD` ファイル）またはルーティングのジョブ（パスと工具、または工具テーブル付きの `.PLT` ファイル）を、プログラムのライブラリで計画します。そのためクライアントが描くものと機械が行うことがまったく同じになります。穴がテーブルの外にある、または工具がない場合は、コード付きで拒否されます。
* **プレビュー**：機械が行う順序での作業、工具とその数、外接ボックス、時間の見積り、各穴が終わる項目。画面が基板を描きジョブを追うのに必要なものです。
* **実行**：ジョブは人が開始するまで準備完了のままです。プログラムのライブラリ上で動き（ハンドシェイク、アーミング、基準復帰、穴あけ、停止）、オペレーターのダイアログ（工具交換、スピンドルの確認）では誰かが確認するまで待ち、停止もできます。機械はドライバーを切って止まります。
* **機械**：それぞれの接続方法（ネットワーク越しの EH3D-NODE-BUNGARD、またはシミュレーター）、設定ファイルのテキスト、その機械で調整した値。機械のキャリブレーションが、すべてのクライアントに対して 1 か所に置かれます。
* **人とプログラム**：3 つの権限を持つユーザー、一度だけ表示されるプログラム用トークン、誤ったパスワードへの遅延、最初の管理者のためにコンソールに出るセットアップ コード。
* **イベント**：ジョブの変更をそのつど送る WebSocket。
* **サービス**：1 つのバイナリと小さなテキストの設定ファイル。既定のポートは 18200（ARMOR や HYDRA-UMC とは別）で、CM5 用の systemd ユニットがあります。
* **計画中でまだ作っていないもの**：クライアント（Android アプリ、Studio、機械の横のタッチ パネル）、サーバー自身の HTTPS、サーバーが消えたときに保持中のジョブを終わらせるためのノードのランプとバッファの利用。

## 🧩 EH3D での役割

EH3D-SERVER は EH3D ファミリーの中心です。プログラムはジョブをここへ送り、サーバーはノード経由で機械と通信し、他のクライアントも同じインターフェースで同じジョブを見て送ります。[EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO) と [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO) のライブラリを使い、[EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD) と通信します。

## 🏛️ アーキテクチャ

<p align="center">
  <img src="images/architecture.svg" alt="EH3D-SERVER のアーキテクチャ：誰が使い、何を持ち、何に依存するか" width="100%">
</p>

このリポジトリの各部分の関係：誰が使い、何を持ち、何に依存するか。図は `docs/architecture.json` から描かれ、コードと照合されます。

## ✨ 機能

* **プレビューがそのまま計画：** プレビューと実際の実行は同じ計画から作られるため、クライアントが描くものが機械の動きになります。
* **ジョブは自動では開始されない：** ジョブは準備完了で作成され、オペレーターだけが開始できます。1 台の機械では 1 度に 1 つ（`409 machine_busy`）。
* **ネットワーク越しのオペレーター ダイアログ：** ジョブは工具交換とスピンドル確認で、クライアントが確認するまで一時停止します。
* **ライブ イベント：** WebSocket がジョブの変更をそのつど知らせ、接続時にはジョブの概要を送ります。
* **キャリブレーションを 1 か所に：** 各機械の設定ファイルと調整値はサーバーに保存されるため、すべてのクライアントが同じ数値で計画します。
* **サーバー停止時に実行中だったジョブは失敗扱い：** 機械がどこにいたか誰にも分からないため、自動で再開しません。
* **安全なファイル：** ユーザー、トークン、機械、ジョブは、一時ファイル経由で書かれる JSON ファイルです。壊れたファイルはエラーとなり、黙ってゼロから再開することはありません。

## 🔒 セキュリティと安全

| 脅威 | 防御 |
|---|---|
| ネットワーク上の誰かがリクエストを送る | ヘルスチェック、初回セットアップ、ログイン以外のすべてのリクエストにトークンが必要です（`Authorization: Bearer`、WebSocket は `?token=`）。3 つの権限：閲覧者は読み取り、オペレーターはジョブを実行、管理者はユーザー・トークン・機械を管理 |
| パスワードの推測 | 誤ったパスワードが 5 回で名前をブロックし、30 秒、続くあいだ 60 秒、120 秒…と延びます（`429 throttled`）。どの部分が違ったかは答えず、未知の名前も既知の名前と同じ時間がかかります |
| データ フォルダーのコピー | パスワードは Argon2id ハッシュ。セッションとプログラムのトークンはランダムで SHA-256 だけを保存するため、フォルダーのコピーで誰もログインできません。ノードのトークンは API が表示することはありません |
| 新しいサーバーの乗っ取り | 最初の管理者は、コンソールだけに表示されるセットアップ コードで作成します |
| 全員を締め出す | 最後の管理者は削除・無効化・降格できません（`409 last_admin`） |
| 勝手に始まるジョブ、または 1 台に 2 つ目のジョブ | ジョブは準備完了で作成され、オペレーターだけが開始でき、1 台の機械は 1 度に 1 つのジョブを実行します |
| テーブルに当たる、または存在しない工具を使う計画 | 計画は、テーブル外の穴やパス、不足または範囲外の工具を、コード付きで拒否します |
| 機械が動いている間にサーバーが落ちる | リンクが途絶えるとノード自身が停止します。ジョブは失敗扱いとなり自動では再開せず、サービスは SIGINT でジョブを止めます |
| 巨大なリクエスト | 本文のサイズは制限されます（`max_body_mb`、既定 8 MB） |
| 壊れたデータ ファイル | エラーとなり、黙ってゼロから始めることはありません |
| 権限が多すぎるサービス | systemd ユニットは専用ユーザーで、`NoNewPrivileges`、`ProtectSystem=strict`、`PrivateTmp`、書き込み可能なフォルダー 1 つで動きます |

**防御していないこと：** サーバーは TLS を終端しないため、前段に HTTPS を置かない限り、トークンとパスワードは平文でネットワークを通ります。WebSocket のトークンはアドレスに含まれます。独立したレビューは受けていません。脆弱性は非公開で報告してください（[SECURITY.md](SECURITY.md) 参照）。

## 🔌 インターフェース

| インターフェース | 内容 | 場所 |
|---|---|---|
| REST | `/api/v1/`：`health`、`setup`、`login`、`logout`、`session`、`account`、`users`、`tokens`、`machines`（`preview` と `jobs` 付き）、`jobs`（`start`、`confirm`、`cancel` 付き）。入出力は JSON、エラーは `{"error","detail"}` | [docs/API.md](docs/API.md)、`src/api.rs` |
| WebSocket | `GET /api/v1/events?token=...`：ジョブの概要を持つ `hello`、その後の変更ごとの `job` と `job_removed` | [docs/API.md](docs/API.md) |
| 機械へ | 機械の `connection` は `node`（アドレスとトークンを持つ EH3D-NODE-BUNGARD）または `demo`（シミュレーター） | [docs/API.md](docs/API.md)、`src/machines.rs` |
| コマンドライン | `eh3d-server [--config FILE]`。`--help` で一覧表示 | `src/main.rs` |
| サービス | 1 つのバイナリ、既定ポート 18200、systemd ユニット | [docs/DEPLOY.md](docs/DEPLOY.md)、`deploy/eh3d-server.service` |

## ⚙️ 設定

`server.cfg`（または `--config` で指定したファイル）には 5 つの省略可能なキーがあります。未知のキーや範囲外の値は起動時にエラーになります。`server.cfg.example` に説明があります。

| キー | 既定値 | 範囲 | 内容 |
|---|---|---|---|
| `listen` | `0.0.0.0:18200` | アドレスとポート | サーバーが待ち受ける場所（18200 は専用ポート） |
| `data_dir` | `data` | フォルダー | ユーザー、トークン、機械、ジョブの保存先。バックアップしてください |
| `session_hours` | 12 | 1 〜 720 | ログインの有効時間 |
| `max_body_mb` | 8 | 1 〜 256 | 最大のリクエスト（メガバイト） |
| `keep_jobs` | 200 | 10 〜 100,000 | 一覧に残す終了済みジョブの数 |

ユーザー名とトークン名：英数字、`.`、`_`、`-`、32 文字まで。パスワード：8 〜 128 文字。各機械で調整した値はここにはなく、機械のレコードにプログラムのマシン ファイルの形式で保存されます。

## 🚀 クイックスタート

安定版の Rust ツールチェーンと、このフォルダーの隣に EH3D-DRILLPRO と EH3D-ROUTEPRO が必要です（サーバーはそれらのライブラリを使います）。

```bash
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
python -I tools/ci_validate.py
```

`run.bat`（Windows）または `./run.sh`（Linux）で起動します。必要なら先にビルドします。ユーザーがいない間、コンソールにセットアップ コードが表示されます。`POST /api/v1/setup` で管理者を作成し、ログインして、ノードのアドレスとトークンで機械を追加し、プログラムごとにトークンを作ります。設定は `server.cfg.example` に一覧があり、CM5 でのサービスとしての導入は `docs/DEPLOY.md` にあります。

## 🧪 テスト

テストは 39 件です（`cargo test`、CI では DRILLPRO と ROUTEPRO をサーバーの隣に取得して Windows と Linux で実行）。モジュール内 28 件（設定とその範囲、ユーザー、権限、トークン、誤ったパスワードの遅延、ファイルとジョブ台帳、計画）、実際のルーターを通すインターフェースのエンドツーエンド 7 件（`tests/api.rs`）、実プログラムが実サーバーへ実ポートでジョブを送り機械はシミュレーターという 4 件（`tests/programs.rs`）です。CI は `cargo fmt --all --check`、警告をエラーとする `cargo clippy`、`python -I tools/ci_validate.py` も実行します。実際のノードや機械に対するテストはありません。

## 🏗️ ビルドとリリース

バージョンは `Cargo.toml` と `eh3d.project.json` にあり、各バージョンには [CHANGELOG.md](CHANGELOG.md) の項目があります。`build.sh` / `build.bat` はリリース バイナリ（`target/release/eh3d-server`）をライブ パネル付きでビルドし、`test.sh` / `test.bat` はテストだけを実行します。どちらもバージョンは変えません。リリースはまだ公開していません。CM5（aarch64 Linux）では機械上で直接ビルドし、`docs/DEPLOY.md` でサービスとして導入します。

## 📂 リポジトリの構成

<p align="center">
  <img src="images/repository-structure.svg" alt="EH3D-SERVER のリポジトリ構成" width="100%">
</p>

<details>
<summary>ツリーのテキスト版</summary>

```text
EH3D-SERVER/
├── src/
│   ├── api.rs                                   REST ルートと WebSocket
│   ├── auth.rs                                  ユーザー、権限、トークン、誤ったパスワードの遅延
│   ├── plan.rs                                  リクエストから機械が実行する項目まで
│   ├── jobs.rs state.rs                         ジョブと、実行中のサーバーの構成要素
│   ├── machines.rs store.rs                     機械と、一時ファイル経由で書かれるファイル
│   └── config.rs lib.rs main.rs                 設定ファイル、ライブラリのルート、プログラム
├── tests/                                       シミュレーター上のインターフェースのエンドツーエンドと、実サーバーに対する実プログラム
├── docs/                                        API、サーバー運用の手引き、2 つの図のデータ
├── deploy/                                      サービスの systemd ユニット
├── server.cfg.example                           すべてのキーに説明を付けた設定ファイル
├── tools/                                       CI と README のチェック、スクリプトのパネル、図のツール
├── images/                                      バナーとアーキテクチャ・リポジトリ構成の図
├── .github/                                     CI とリリースのワークフロー、Issue と PR のテンプレート
├── build.sh build.bat                           ライブ パネルでリリース バイナリをビルド
├── run.sh run.bat                               サーバーを起動（必要なら先にビルド）
├── eh3d.project.json                            マニフェスト：バージョン、役割、成熟度、ビルドコマンド
├── Cargo.toml Cargo.lock                        Rust パッケージ、依存関係、2 つのプログラムのライブラリ
├── CHANGELOG.md                                 各バージョンの変更点
├── SECURITY.md SUPPORT.md                       脆弱性の報告方法と問い合わせ先
└── CONTRIBUTING.md CODE_OF_CONDUCT.md LICENSE   貢献方法、行動規範、ライセンス
```

</details>

## 🗺️ ロードマップと既知の制限

**現時点で分かっている制限**

* サーバー経由で実機を動かしたことはありません。ジョブはシミュレーターに対してのみ実行され、EH3D-NODE-BUNGARD へのリンクも実際には到達していません。
* サーバーは TLS を終端しません。外部から到達する前に HTTPS を前段に置いてください。独立したレビューは受けていません。
* プログラム以外のクライアント（Android アプリ、Studio、タッチ パネル）はまだ存在しません。
* EH3D-DRILLPRO と EH3D-ROUTEPRO のライブラリを使うため、そのフォルダーが隣に必要です。
* サーバー停止時に実行中だったジョブは失敗扱いとなり、再開しません。

**次のステップ**

* サーバーとノードを通して、実機で 1 つのジョブを最後まで実行する。
* サーバー自身で HTTPS に対応する。
* サーバーが消えたときに保持中のジョブを終わらせるため、ノードのランプとバッファを使う。
* プログラム以外の最初のクライアントを作る。

## 🌍 言語

README とテキストは、英語、スペイン語、フランス語、イタリア語、ドイツ語、中国語、日本語、ヨーロッパポルトガル語で提供されています。

## 🔗 関連プロジェクト

* [EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO)：ドリル プログラム。そのライブラリがドリル ジョブを計画・実行します
* [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO)：ルーティング プログラム。そのライブラリがルーティング ジョブを計画します
* [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD)：サーバーが通信する、機械の筐体内のノード
* [EH3D-COMMON](https://github.com/JuanenRac/EH3D-COMMON)：ファミリー共通の部分

## 📚 ドキュメント

* [API](docs/API.md)：すべてのリクエスト、ジョブとクライアントが描くビュー、状態、WebSocket。
* [サーバーの運用](docs/DEPLOY.md)：ビルド、初回起動、CM5 上のサービス、配備の安全性。
* [変更履歴](CHANGELOG.md)

## 🤝 コントリビュートとコミュニティ

* プルリクエストの前に `cargo fmt --all --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`、`python -I tools/ci_validate.py` を実行し、すべて通してください。[CONTRIBUTING.md](CONTRIBUTING.md) と[行動規範](CODE_OF_CONDUCT.md)を参照してください。
* README は 8 言語あります。文章を変更するときは 8 つのファイルすべてを変更し、2 つの図は `docs/architecture.json` と `docs/structure.json` から `tools/make_architecture.py` と `tools/make_structure.py` で描画します。
* API のルートを変更するときは、`docs/API.md` に項目を追加し、実際のルーターを通すテストを書き、何かを変更するリクエストには実行できる権限を定めてください。
* 脆弱性は非公開で報告してください：[SECURITY.md](SECURITY.md)。質問、アイデア、報告：electrohobby3d@gmail.com（[SUPPORT.md](SUPPORT.md)）。

## 👤 作者

**JuanenRac (Electro Hobby 3D)** · electrohobby3d@gmail.com

## 📜 ライセンス

ソフトウェア：GPL-3.0-or-later。[LICENSE](LICENSE) を参照。スイートのハードウェア設計には CERN OHL-S を使います。
