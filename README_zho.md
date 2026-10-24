<p align="center">
  <img src="images/EH3D_BANNER.svg" alt="EH3D-SERVER banner" width="100%">
</p>

# 🧭 EH3D-SERVER

<p align="center"><a href="README.md">🇺🇸 English</a> | <a href="README_spa.md">🇪🇸 Español</a> | <a href="README_fra.md">🇫🇷 Français</a> | <a href="README_ita.md">🇮🇹 Italiano</a> | <a href="README_deu.md">🇩🇪 Deutsch</a> | 🇨🇳 <b>中文</b> | <a href="README_jpn.md">🇯🇵 日本語</a> | <a href="README_por.md">🇵🇹 Português</a></p>

### EH3D 程序的中心：接收来自 DRILLPRO、ROUTEPRO 和今后客户端的作业，规划它们，通过机器的节点在机器上执行，并告诉所有人进展如何

<p align="center">
  <img src="https://img.shields.io/badge/License-GPL%203.0--or--later-blue.svg" alt="GPL 3.0 or later">
  <img src="https://img.shields.io/badge/Stack-Rust-DEA584.svg" alt="Rust">
  <img src="https://img.shields.io/badge/Languages-8-00E5FF.svg" alt="Languages">
  <img src="https://img.shields.io/badge/Version-0.0.1-0f6cbd.svg" alt="Version">
  <img src="https://img.shields.io/badge/Maturity-scaffolding-FFB020.svg" alt="Maturity">
</p>

---

**诚实性核对 - 今天真正能运行的内容：** 一个 Rust 服务器（axum），带 REST 和 websocket 接口，保存用户、机器和作业，用 EH3D-DRILLPRO 和 EH3D-ROUTEPRO 的库规划钻孔或铣削作业（孔和路径，或 Excellon / HP-GL 文件及其刀具表），显示预览，并通过机器的 EH3D-NODE-BUNGARD 执行，带操作员对话框、停止，以及实时通报的每一项变化。电脑上运行 39 项测试，作业对着模拟器运行。**还没有通过它在真实机器上运行过任何东西**，它不终结 TLS，而且除程序之外将使用它的客户端（Android 应用、Studio、机器旁的面板）还不存在。

---

> **安全（safety）与信息安全现状：** 这不是安全系统。作业绝不会自行开始：由具有操作员角色的人启动，每台机器一次只运行一个，规划会拒绝台面之外的孔和路径以及缺失或超出范围的刀具。每个请求都需要令牌（查看者只读，操作员运行作业，管理员管理用户、令牌和机器）；密码以 Argon2 哈希保存，会话和程序令牌只以 SHA-256 保存，节点的令牌 API 从不显示。服务器适用于家中或通向家的 VPN：在外部能访问之前请在前面加上 HTTPS。机器的停止并不只依赖服务器：链路沉默时节点会自己停止。本项目是独立的：它不是 Bungard Elektronik 制造、认可或支持的。

## 📋 概览

| | |
|---|---|
| 版本 | `0.0.1` |
| 成熟度 | 脚手架 |
| 测试 | 39 |
| 技术栈 | Rust |
| 真实硬件 | 否：已对着模拟器测试；还没有通过服务器在真实机器上运行过任何东西 |

## 🚦 项目状态

每一行是本仓库的一个部分，灯号表示它今天做到了哪一步。内容依据仓库能够展示的证据（测试、构建、运行），而不是计划。

🟢 已完成并验证 · 🟡 可用，有已知限制 · 🟠 部分完成或尚未验证 · 🔴 未完成或不可用

| 领域 | 状态 | 在这里的含义 |
|---|---|---|
| 软件 | 🟡 | Rust 服务器（axum）保存用户、机床和作业，用 EH3D-DRILLPRO 和 EH3D-ROUTEPRO 的库规划钻孔和铣削作业，显示预览，并带着操作员对话框和停止功能运行作业。 |
| 测试 | 🟢 | 39 项测试通过（`cargo test`），接口做了端到端测试，作业在模拟器上运行。 |
| 面向客户端的接口（API） | 🟡 | REST 请求和 websocket 事件写在 `docs/API.md` 中，并有测试覆盖；目前只有各程序自带的服务器连接在使用它们。 |
| 客户端 | 🔴 | Android 应用、Studio 以及机床旁的触摸面板都还不存在。 |
| 安全性 | 🟡 | 每个请求都需要带三种角色之一的令牌，密码以 Argon2 哈希保存，输错密码会被限速；服务器本身不终止 TLS（需要在前面放 HTTPS），也没有经过独立审查。 |
| 真实硬件 | 🟠 | 作业只在模拟器上运行过：还没有通过服务器在真实机床上运行任何东西。 |
| 部署 | 🟠 | systemd 单元和 `docs/DEPLOY.md` 描述了在 CM5 上的服务；没有记录显示已经部署过。 |
| 集成 | 🟡 | 它使用与 EH3D-DRILLPRO 和 EH3D-ROUTEPRO 相同的库进行规划（它们的文件夹必须放在本文件夹旁边），并与 EH3D-NODE-BUNGARD 的链路通信，但只通过模拟器触达过。 |
| 文档 | 🟢 | 八种语言的 README、API 参考以及作为服务运行的指南。 |

## 🎯 简介

* **作业**：钻孔作业（孔和刀具，或 `.NCD` 文件及其刀具表）或铣削作业（路径和刀具，或 `.PLT` 文件及其刀具表），用程序的库规划，因此客户端绘制的和机器执行的完全一样；孔在台面之外或缺少刀具时会带代码被拒绝。
* **预览**：按机器执行顺序排列的作业、带数量的刀具、外接矩形、时间估计，以及每个孔完成时对应的项目——屏幕绘制电路板和跟踪作业所需的一切。
* **运行**：作业在有人启动之前一直处于就绪状态；它在程序的库上运行（握手、解锁、回参考点、钻孔、停止），在操作员对话框（换刀、确认主轴）处等待直到有人确认，并且可以停止；机器在驱动器关闭的情况下停下。
* **机器**：每台机器如何到达（网络上的 EH3D-NODE-BUNGARD，或模拟器）、其配置文件的文本和在其上调好的数值，因此机器的校准对所有客户端都集中在一处。
* **人和程序**：三种角色的用户、只显示一次的程序令牌、对错误密码的减速，以及控制台上给第一位管理员的初始设置代码。
* **事件**：一个 websocket，在作业发生变化时立即发送。
* **服务**：一个二进制文件和一个小的文本设置文件，默认端口 18200（不是 ARMOR 或 HYDRA-UMC 的），并带有用于 CM5 的 systemd 单元。
* **计划中，尚未实现**：客户端（Android 应用、Studio、机器旁的触摸面板）、服务器自身的 HTTPS，以及利用节点的指示灯和缓冲区，在服务器消失时完成已持有的作业。

## 🧩 在 EH3D 中的角色

EH3D-SERVER 是 EH3D 家族的中心：程序把作业发给它，它通过节点与机器通信，其他客户端通过同一接口查看和发送同样的作业。它使用 [EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO) 和 [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO) 的库，并与 [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD) 通信。

## 📂 仓库结构

```text
EH3D-SERVER/
├── src/          服务器：设置、用户、机器、规划、作业、接口和 websocket
├── tests/        端到端的接口测试，作业在模拟器上运行
├── docs/         API.md 和 DEPLOY.md
├── deploy/       systemd 单元
├── tools/        ci_validate.py：清单、版本、变更日志和 README 检查
├── images/       横幅
├── server.cfg.example  设置文件示例: 复制为 server.cfg 使用 (每个键都是可选的)
└── eh3d.project.json
```

## 🛠️ 构建与运行

需要稳定版 Rust 工具链，并且 EH3D-DRILLPRO 和 EH3D-ROUTEPRO 要放在本文件夹旁边。

```bash
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
python -I tools/ci_validate.py
```

用 `run.bat`（Windows）或 `./run.sh`（Linux）运行；需要时会先编译。没有用户时控制台会显示初始设置代码：用 `POST /api/v1/setup` 创建管理员，登录，用节点的地址和令牌添加机器，并为每个程序创建一个令牌。`server.cfg.example` 列出设置项；`docs/DEPLOY.md` 说明如何在 CM5 上安装为服务。

## 🔗 相关项目

* [EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO): 钻孔程序；它的库规划并执行钻孔作业
* [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO): 铣削程序；它的库规划铣削作业
* [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD): 装在机器箱内、服务器与之通信的节点
* [EH3D-COMMON](https://github.com/JuanenRac/EH3D-COMMON): 家族的公共部分

## 📚 文档与社区

* [API](docs/API.md): 每个请求、作业和客户端绘制的视图、状态以及 websocket.
* [运行服务器](docs/DEPLOY.md): 构建、首次启动、CM5 上的服务以及部署的安全性.
* [更新日志](CHANGELOG.md)
* [参与贡献](CONTRIBUTING.md)
* [安全政策](SECURITY.md)
* [支持](SUPPORT.md)

## 👤 作者

**JuanenRac (Electro Hobby 3D)** · electrohobby3d@gmail.com

## 📜 许可证

软件：GPL-3.0-or-later。见 [LICENSE](LICENSE)。套件的硬件设计使用 CERN OHL-S。
