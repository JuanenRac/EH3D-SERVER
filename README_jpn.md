<p align="center">
  <img src="images/EH3D_BANNER.svg" alt="EH3D-SERVER banner" width="100%">
</p>

# 🧭 EH3D-SERVER

<p align="center"><a href="README.md">🇺🇸 English</a> | <a href="README_spa.md">🇪🇸 Español</a> | <a href="README_fra.md">🇫🇷 Français</a> | <a href="README_ita.md">🇮🇹 Italiano</a> | <a href="README_deu.md">🇩🇪 Deutsch</a> | <a href="README_zho.md">🇨🇳 中文</a> | 🇯🇵 <b>日本語</b> | <a href="README_por.md">🇵🇹 Português</a></p>

### EH3D プログラムの中心：DRILLPRO、ROUTEPRO、今後のクライアントからの作業を受け取り、計画し、ノード経由で機械上で実行し、進み具合を全員に伝えます

<p align="center">
  <img src="https://img.shields.io/badge/License-GPL%203.0--or--later-blue.svg" alt="GPL 3.0 or later">
  <img src="https://img.shields.io/badge/Stack-Rust-DEA584.svg" alt="Rust">
  <img src="https://img.shields.io/badge/Languages-8-00E5FF.svg" alt="Languages">
  <img src="https://img.shields.io/badge/Version-0.0.1-0f6cbd.svg" alt="Version">
  <img src="https://img.shields.io/badge/Maturity-scaffolding-FFB020.svg" alt="Maturity">
</p>

---

**正直さの確認 - 今日実際に動くもの：** ユーザー、機械、作業を保持し、EH3D-DRILLPRO と EH3D-ROUTEPRO のライブラリで穴あけまたはルーティングの作業（穴と経路、または Excellon / HP-GL ファイルと工具テーブル）を計画し、プレビューを示し、機械の EH3D-NODE-BUNGARD 経由で実行する Rust サーバー（axum）です。REST と websocket のインターフェースを持ち、オペレーターのダイアログ、停止、すべての変更のリアルタイム通知に対応します。パソコン上で 39 件のテストが動き、作業はシミュレーターに対して実行されます。**これを通して実機で動かしたことはまだありません**。TLS は終端せず、プログラム以外にこれを使うクライアント（Android アプリ、Studio、機械脇のパネル）もまだ存在しません。

---

> **安全性とセキュリティの現状：** これは安全システムではありません。作業は決して自動では始まりません。オペレーターの役割を持つ人が開始し、1 台の機械につき一度に 1 つで、計画は台の外の穴や経路、不足または範囲外の工具を拒否します。すべてのリクエストにトークンが必要です（閲覧者は読み取り、オペレーターは作業を実行、管理者はユーザー・トークン・機械を管理）。パスワードは Argon2 ハッシュ、セッションとプログラムのトークンは SHA-256 のみで保存され、ノードのトークンは API が決して表示しません。サーバーは家庭内または VPN 用です。外部から届く前に HTTPS を前段に置いてください。機械の停止はサーバーだけには依存しません。リンクが無言になるとノードが自分で停止します。このプロジェクトは独立しており、Bungard Elektronik が製造・推奨・サポートするものではありません。

## 📋 概要

| | |
|---|---|
| バージョン | `0.0.1` |
| 成熟度 | 足場段階 |
| テスト | 39 |
| 技術スタック | Rust |
| 実機での動作 | いいえ：シミュレーターでテスト済み。サーバー経由での実機の動作はまだありません |

## 🚦 プロジェクトの状況

各行はこのリポジトリの一部分で、信号は今日どこまで進んでいるかを示します。計画ではなく、リポジトリが示せるもの（テスト、ビルド、実行）に基づいて書かれています。

🟢 完了・検証済み · 🟡 動作する（既知の制限あり） · 🟠 一部のみ、または未検証 · 🔴 未実装または動作しない

| 領域 | 状態 | ここでの意味 |
|---|---|---|
| ソフトウェア | 🟡 | Rust 製サーバー（axum）がユーザー、マシン、ジョブを管理し、EH3D-DRILLPRO と EH3D-ROUTEPRO のライブラリで穴あけ・ルーティングのジョブを計画し、プレビューを表示して、オペレーターのダイアログと停止付きで実行します。 |
| テスト | 🟢 | 39 件のテストが通ります（`cargo test`）。シミュレータ上でジョブを動かし、インターフェースを端から端まで確認しています。 |
| クライアント向けインターフェース（API） | 🟡 | REST リクエストと WebSocket イベントは `docs/API.md` に書かれ、テストで確認されています。今のところ使っているのはプログラム自身のサーバー接続だけです。 |
| クライアント | 🔴 | Android アプリ、Studio、マシンの横のタッチパネルはまだ存在しません。 |
| セキュリティ | 🟡 | すべてのリクエストに 3 つのロールのいずれかを持つトークンが必要で、パスワードは Argon2 ハッシュ、誤ったパスワードは遅延されます。サーバー自体は TLS を終端しない（前段に HTTPS が必要）ため、独立したレビューも受けていません。 |
| 実機 | 🟠 | ジョブはシミュレータ上でしか動いていません。サーバー経由で実機が動いたことはありません。 |
| デプロイ | 🟠 | systemd ユニットと `docs/DEPLOY.md` が CM5 上のサービスを説明していますが、実際にデプロイした記録はありません。 |
| 連携 | 🟡 | EH3D-DRILLPRO・EH3D-ROUTEPRO と同じライブラリで計画します（これらのフォルダをこのフォルダの隣に置く必要があります）。EH3D-NODE-BUNGARD のリンクとも通信しますが、到達したのはシミュレータ経由だけです。 |
| ドキュメント | 🟢 | 8 言語の README、API リファレンス、サービスとして動かすためのガイド。 |

## 🎯 概要説明

* **作業**：穴あけ作業（穴と工具、または `.NCD` ファイルと工具テーブル）またはルーティング作業（経路と工具、または `.PLT` ファイルと工具テーブル）を、プログラムのライブラリで計画します。クライアントが描くものと機械が行うものはまったく同じです。穴がテーブルの外にある、または工具が足りない場合はコード付きで拒否します。
* **プレビュー**：機械が行う順序での作業、工具とその数、全体の外接矩形、時間の見積もり、各穴が完了する項目。基板を描画して作業を追うために画面が必要とするものです。
* **実行**：作業は人が開始するまで待機状態です。プログラムのライブラリ（ハンドシェイク、アーム、原点出し、穴あけ、停止）上で動き、オペレーターのダイアログ（工具交換、スピンドルの確認）で誰かが確認するまで待ち、停止もできます。機械はドライバをオフにして停止します。
* **機械**：それぞれの機械への接続方法（ネットワーク上の EH3D-NODE-BUNGARD、またはシミュレーター）、設定ファイルのテキスト、機械で調整した値。機械のキャリブレーションはすべてのクライアントに対して 1 か所にあります。
* **人とプログラム**：3 つの役割を持つユーザー、一度だけ表示されるプログラム用トークン、誤ったパスワードに対する遅延、最初の管理者のためにコンソールに表示されるセットアップコード。
* **イベント**：作業の変更をその場で送る websocket。
* **サービス**：1 つのバイナリと小さな設定テキストファイル。既定のポートは 18200（ARMOR や HYDRA-UMC とは別）で、CM5 用の systemd ユニット付きです。
* **予定（未実装）**：クライアント（Android アプリ、Studio、機械脇のタッチパネル）、サーバー自身での HTTPS、サーバーがいなくなっても保持している作業を終えるためのノードのランプとバッファの活用。

## 🧩 EH3D での役割

EH3D-SERVER は EH3D ファミリーの中心です。プログラムは作業をここに送り、サーバーはノード経由で機械と話し、他のクライアントも同じインターフェースで同じ作業を見て送ります。[EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO) と [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO) のライブラリを使い、[EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD) と通信します。

## 📂 リポジトリ構成

```text
EH3D-SERVER/
├── src/          サーバー本体：設定、ユーザー、機械、計画、作業、インターフェース、websocket
├── tests/        インターフェースのエンドツーエンドテスト（作業はシミュレーターで実行）
├── docs/         API.md と DEPLOY.md
├── deploy/       systemd ユニット
├── tools/        ci_validate.py：マニフェスト、バージョン、変更履歴、README の確認
├── images/       バナー
├── server.cfg.example  設定ファイルの例: server.cfg にコピーして使う (どのキーも任意)
└── eh3d.project.json
```

## 🛠️ ビルドと実行

安定版の Rust ツールチェーンと、このフォルダーの隣に EH3D-DRILLPRO と EH3D-ROUTEPRO が必要です。

```bash
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
python -I tools/ci_validate.py
```

`run.bat`（Windows）または `./run.sh`（Linux）で起動します。必要なら先にビルドします。ユーザーがいない間、コンソールにセットアップコードが表示されます。`POST /api/v1/setup` で管理者を作成し、ログインして、ノードのアドレスとトークンで機械を追加し、プログラムごとにトークンを作成します。`server.cfg.example` に設定の一覧があり、`docs/DEPLOY.md` で CM5 のサービスとしてインストールできます。

## 🔗 関連プロジェクト

* [EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO): 穴あけプログラム。そのライブラリが穴あけ作業を計画・実行します
* [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO): ルーティングプログラム。そのライブラリがルーティング作業を計画します
* [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD): 機械の筐体内にあり、サーバーが通信するノード
* [EH3D-COMMON](https://github.com/JuanenRac/EH3D-COMMON): ファミリーの共通部分

## 📚 ドキュメントとコミュニティ

* [API](docs/API.md): すべてのリクエスト、作業とクライアントが描画するビュー、状態、websocket.
* [サーバーの運用](docs/DEPLOY.md): ビルド、初回起動、CM5 でのサービス、導入時の安全性.
* [変更履歴](CHANGELOG.md)
* [貢献方法](CONTRIBUTING.md)
* [セキュリティポリシー](SECURITY.md)
* [サポート](SUPPORT.md)

## 👤 作者

**JuanenRac (Electro Hobby 3D)** · electrohobby3d@gmail.com

## 📜 ライセンス

ソフトウェア：GPL-3.0-or-later。[LICENSE](LICENSE) を参照。スイートのハードウェア設計には CERN OHL-S を使います。
