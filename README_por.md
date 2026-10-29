<p align="center">
  <img src="images/EH3D_BANNER.svg" alt="EH3D-SERVER banner" width="100%">
</p>

# 🧭 EH3D-SERVER

<p align="center"><a href="README.md">🇺🇸 English</a> | <a href="README_spa.md">🇪🇸 Español</a> | <a href="README_fra.md">🇫🇷 Français</a> | <a href="README_ita.md">🇮🇹 Italiano</a> | <a href="README_deu.md">🇩🇪 Deutsch</a> | <a href="README_zho.md">🇨🇳 中文</a> | <a href="README_jpn.md">🇯🇵 日本語</a> | 🇵🇹 <b>Português</b></p>

### O centro dos programas EH3D: recebe trabalhos do DRILLPRO, do ROUTEPRO e dos clientes futuros, planeia-os, executa-os na máquina através do seu nó e conta a todos como correm

<p align="center">
  <img src="https://img.shields.io/badge/License-GPL%203.0--or--later-blue.svg" alt="GPL 3.0 or later">
  <img src="https://img.shields.io/badge/Stack-Rust-DEA584.svg" alt="Rust">
  <img src="https://img.shields.io/badge/Languages-8-00E5FF.svg" alt="Languages">
  <img src="https://img.shields.io/badge/Version-0.0.1-0f6cbd.svg" alt="Version">
  <img src="https://img.shields.io/badge/Maturity-scaffolding-FFB020.svg" alt="Maturity">
</p>

---

**Verificação de honestidade - o que funciona hoje:** um servidor em Rust (axum) com interface REST e websocket que guarda utilizadores, máquinas e trabalhos, planeia um trabalho de furação ou de fresagem (furos e trajetórias, ou um ficheiro Excellon ou HP-GL com a sua tabela de ferramentas) com as bibliotecas do EH3D-DRILLPRO e do EH3D-ROUTEPRO, mostra uma pré-visualização e executa-o numa máquina através do seu EH3D-NODE-BUNGARD ou num simulador, com os diálogos do operador, uma paragem e cada alteração comunicada à medida que acontece. 39 testes correm num computador, os trabalhos contra o simulador. **Ainda nada correu numa máquina real através dele**, não termina TLS e os clientes que o usarão além dos programas (a app Android, o Studio, o painel junto à máquina) ainda não existem.

---

> **Estado de segurança (safety) e de segurança informática:** isto não é um sistema de segurança (safety). Um trabalho nunca arranca sozinho: arranca-o uma pessoa com o papel de operador, um trabalho de cada vez numa máquina, e o planeamento recusa furos e trajetórias fora da mesa e ferramentas em falta ou fora de gama. Cada pedido precisa de um token (um observador lê, um operador executa trabalhos, um administrador gere utilizadores, tokens e máquinas); as palavras-passe guardam-se como hashes Argon2, os tokens de sessão e de programa só como SHA-256, e o token de um nó nunca é mostrado pela API. O servidor é para a casa ou uma VPN para ela: ponha HTTPS à frente antes que algo de fora lhe chegue. A paragem da máquina não depende só do servidor: o nó pára sozinho quando a sua ligação se cala. O projeto é independente: não é feito, aprovado nem apoiado pela Bungard Elektronik.

## 📋 Num relance

| | |
|---|---|
| Versão | `0.0.1` |
| Maturidade | Estrutura inicial |
| Testes | 39 |
| Tecnologias | Rust (axum, tokio) |
| Em hardware real | Não: testado contra o simulador; ainda nada correu numa máquina real através do servidor |

**Ir para:** [Visão geral](#-visão-geral) · [Arquitetura](#-arquitetura) · [Início rápido](#-início-rápido) · [Testes](#-testes) · [Estrutura](#-estrutura-do-repositório) · [Relacionados](#-projetos-relacionados)

## 🚦 Estado do projeto

Cada linha é uma parte deste repositório e a luz diz até onde chega hoje. É escrita com o que o repositório consegue demonstrar (testes, compilações, execuções), não com o que está previsto.

🟢 Feito e verificado · 🟡 Funciona, com limites conhecidos · 🟠 Feito a meio ou ainda por verificar · 🔴 Por fazer ou a não funcionar

| Área | Estado | O que significa aqui |
|---|---|---|
| Software | 🟡 | Um servidor em Rust (axum) guarda utilizadores, máquinas e trabalhos, planeia trabalhos de furação e fresagem com as bibliotecas do EH3D-DRILLPRO e do EH3D-ROUTEPRO, mostra uma pré-visualização e executa o trabalho com diálogos de operador e uma paragem. |
| Testes | 🟢 | Passam 39 testes (`cargo test`): 28 nos módulos e 11 sobre a interface de ponta a ponta, com trabalhos no simulador (7 em `tests/api.rs`, 4 com os programas reais em `tests/programs.rs`). |
| Interface para clientes (API) | 🟡 | Os pedidos REST e os eventos websocket estão descritos em `docs/API.md` e cobertos pelos testes; até agora só a ligação ao servidor dos próprios programas os usa. |
| Clientes | 🔴 | A app Android, o Studio e o painel tátil junto à máquina ainda não existem. |
| Segurança | 🟡 | Cada pedido precisa de um token com um de três papéis, as palavras-passe são hashes Argon2 e as palavras-passe erradas são abrandadas; o servidor não termina TLS (ponha HTTPS à frente) e não teve revisão independente. |
| Hardware real | 🟠 | Os trabalhos só correram contra o simulador: nada correu numa máquina real através do servidor. |
| Instalação | 🟠 | Uma unidade systemd e `docs/DEPLOY.md` descrevem o serviço na CM5; este repositório não regista nenhuma instalação como feita. |
| Integração | 🟡 | Planeia com as mesmas bibliotecas do EH3D-DRILLPRO e do EH3D-ROUTEPRO (as suas pastas têm de estar junto a esta) e fala com a ligação do EH3D-NODE-BUNGARD, que só alcançou através do simulador. |
| Documentação | 🟢 | README em oito idiomas, a referência da API e o guia para o executar como serviço. |

## 🎯 Visão geral

* Os **trabalhos**: um trabalho de furação (furos e ferramentas, ou um ficheiro `.NCD` com a sua tabela de ferramentas) ou de fresagem (trajetórias e ferramentas, ou um ficheiro `.PLT` com a sua tabela de ferramentas), planeado com as bibliotecas dos programas, de modo que o cliente desenha e a máquina faz exatamente o mesmo; recusado com um código quando um furo fica fora da mesa ou falta uma ferramenta.
* A **pré-visualização**: o trabalho na ordem em que a máquina o faz, as ferramentas com a sua contagem, a caixa envolvente, uma estimativa do tempo e o elemento em que cada furo termina - o que um ecrã precisa para desenhar a placa e seguir o trabalho.
* A **execução**: um trabalho está pronto até uma pessoa o arrancar; corre na biblioteca dos programas (o handshake, o armamento, a referência, os furos, a paragem), espera nos diálogos do operador (mudar a ferramenta, confirmar o fuso) até alguém confirmar, e pode ser parado; a máquina pára com os drivers desligados.
* As **máquinas**: de cada uma, como chegar a ela (um EH3D-NODE-BUNGARD pela rede, ou o simulador), o texto dos seus ficheiros de configuração e os valores afinados nela, de modo que a calibração de uma máquina vive num só sítio para todos os clientes.
* As **pessoas e os programas**: utilizadores com três papéis, tokens para programas mostrados uma só vez, um abrandamento perante palavras-passe erradas e um código de configuração na consola para o primeiro administrador.
* Os **eventos**: um websocket que envia cada alteração de um trabalho à medida que acontece.
* O **serviço**: um binário e um pequeno ficheiro de texto de definições, na porta 18200 por omissão (não a do ARMOR nem a do HYDRA-UMC), com uma unidade systemd para a CM5.
* **Previsto, ainda por construir**: os clientes (a app Android, o Studio, o painel tátil junto à máquina), HTTPS no próprio servidor e o uso dos LED do nó e do seu buffer para terminar um trabalho retido quando o servidor desaparece.

## 🧩 Papel no EH3D

O EH3D-SERVER é o centro da família EH3D: os programas enviam-lhe os seus trabalhos, fala com as máquinas através dos seus nós, e os outros clientes veem e enviam os mesmos trabalhos pela mesma interface. Usa as bibliotecas do [EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO) e do [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO) e fala com o [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD).

## 🏛️ Arquitetura

<p align="center">
  <img src="images/architecture.svg" alt="Arquitetura de EH3D-SERVER: quem o usa, o que contém e de que depende" width="100%">
</p>

Como as partes deste repositório se encaixam: quem o usa, o que contém e de que depende. A imagem é desenhada a partir de `docs/architecture.json` e verificada com o código.

## ✨ Funcionalidades

* **Uma pré-visualização que é o plano:** a pré-visualização e a execução real saem do mesmo planeamento, por isso o que o cliente desenha é o que a máquina faz.
* **Um trabalho nunca arranca sozinho:** é criado pronto e só um operador o arranca; um trabalho de cada vez numa máquina (`409 machine_busy`).
* **Diálogos do operador pela rede:** o trabalho pára na mudança de ferramenta e na confirmação do fuso até um cliente confirmar.
* **Eventos em direto:** um websocket comunica cada alteração de um trabalho à medida que acontece, com os resumos dos trabalhos ao ligar.
* **Calibração num só sítio:** os ficheiros de configuração e os valores afinados de cada máquina ficam no servidor, por isso todos os clientes planeiam com os mesmos números.
* **Um trabalho que corria quando o servidor parou é marcado como falhado:** ninguém sabe onde estava a máquina, por isso nada retoma sozinho.
* **Ficheiros seguros:** utilizadores, tokens, máquinas e trabalhos são ficheiros JSON escritos através de um temporário; um ficheiro danificado é um erro, nunca um reinício silencioso do zero.

## 🔒 Segurança e proteção

| Ameaça | Defesa |
|---|---|
| Qualquer pessoa na rede a enviar pedidos | cada pedido, exceto a verificação de saúde, a primeira configuração e o início de sessão, precisa de um token (`Authorization: Bearer`, ou `?token=` para o websocket); três papéis: o observador lê, o operador executa trabalhos, o administrador gere utilizadores, tokens e máquinas |
| Adivinhar uma palavra-passe | cinco palavras-passe erradas bloqueiam o nome, 30 s e depois 60 s, 120 s e assim por diante enquanto continuarem (`429 throttled`); a resposta não diz que parte estava errada, e um nome desconhecido custa tanto como um conhecido |
| Uma cópia da pasta de dados | as palavras-passe são hashes Argon2id; os tokens de sessão e de programa são aleatórios e só se guarda o seu SHA-256, por isso uma cópia da pasta não inicia sessão a ninguém; o token de um nó nunca é mostrado pela API |
| Tomar conta de um servidor novo | o primeiro administrador cria-se com um código de configuração que só a consola imprime |
| Deixar toda a gente de fora | o último administrador não pode ser apagado, desativado nem despromovido (`409 last_admin`) |
| Um trabalho que arranca sozinho ou um segundo trabalho numa máquina | um trabalho é criado pronto; só um operador o arranca, e uma máquina executa um trabalho de cada vez |
| Um plano que bateria na mesa ou usaria uma ferramenta em falta | o planeamento recusa com um código furos e trajetórias fora da mesa e ferramentas em falta ou fora de gama |
| O servidor morre enquanto a máquina se move | o nó pára sozinho quando a sua ligação se cala; o trabalho é marcado como falhado e nunca retoma sozinho; o serviço pára os seus trabalhos com SIGINT |
| Um pedido enorme | o corpo é limitado (`max_body_mb`, 8 MB por omissão) |
| Um ficheiro de dados danificado | um erro, nunca um arranque silencioso do zero |
| Um serviço com direitos a mais | a unidade systemd corre com o seu próprio utilizador, com `NoNewPrivileges`, `ProtectSystem=strict`, `PrivateTmp` e uma única pasta gravável |

**O que não é defendido:** o servidor não termina TLS, por isso tokens e palavras-passe atravessam a rede em claro a menos que se ponha HTTPS à frente; o token do websocket vai no endereço; não teve revisão independente. Comunique uma vulnerabilidade em privado (ver [SECURITY.md](SECURITY.md)).

## 🔌 Interfaces

| Interface | O que é | Onde |
|---|---|---|
| REST | `/api/v1/`: `health`, `setup`, `login`, `logout`, `session`, `account`, `users`, `tokens`, `machines` (com `preview` e `jobs`), `jobs` (com `start`, `confirm`, `cancel`); JSON à entrada e à saída, erros como `{"error","detail"}` | [docs/API.md](docs/API.md), `src/api.rs` |
| Websocket | `GET /api/v1/events?token=...`: um `hello` com os resumos dos trabalhos, depois `job` e `job_removed` a cada alteração | [docs/API.md](docs/API.md) |
| Para a máquina | a `connection` de uma máquina é `node` (um EH3D-NODE-BUNGARD num endereço com o seu token) ou `demo` (o simulador) | [docs/API.md](docs/API.md), `src/machines.rs` |
| Linha de comandos | `eh3d-server [--config FICHEIRO]`; `--help` lista-a | `src/main.rs` |
| Serviço | um binário, porta 18200 por omissão, uma unidade systemd | [docs/DEPLOY.md](docs/DEPLOY.md), `deploy/eh3d-server.service` |

## ⚙️ Configuração

`server.cfg` (ou o ficheiro dado com `--config`) tem cinco chaves opcionais; uma chave desconhecida ou um valor fora de gama é um erro ao arrancar. `server.cfg.example` explica-as.

| Chave | Por omissão | Gama | O que faz |
|---|---|---|---|
| `listen` | `0.0.0.0:18200` | um endereço e uma porta | onde o servidor escuta (18200 é a sua porta) |
| `data_dir` | `data` | uma pasta | onde se guardam utilizadores, tokens, máquinas e trabalhos; faça cópia de segurança |
| `session_hours` | 12 | de 1 a 720 | quanto dura um início de sessão |
| `max_body_mb` | 8 | de 1 a 256 | o maior pedido, em megabytes |
| `keep_jobs` | 200 | de 10 a 100 000 | quantos trabalhos terminados a lista guarda |

Nomes de utilizadores e tokens: letras, dígitos, `.`, `_` e `-`, até 32 carateres; palavras-passe: de 8 a 128 carateres. Os valores afinados em cada máquina não estão aqui: vivem no registo da máquina, no formato do ficheiro de máquina dos programas.

## 🚀 Início rápido

Precisa de uma toolchain Rust estável, e do EH3D-DRILLPRO e do EH3D-ROUTEPRO junto a esta pasta (o servidor usa as suas bibliotecas).

```bash
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
python -I tools/ci_validate.py
```

Execute-o com `run.bat` (Windows) ou `./run.sh` (Linux); compila primeiro se for preciso. Enquanto não tem utilizadores, a consola mostra um código de configuração: crie o administrador com `POST /api/v1/setup`, inicie sessão, acrescente a máquina com o endereço e o token do seu nó e crie um token para cada programa. `server.cfg.example` lista as definições; `docs/DEPLOY.md` instala-o como serviço na CM5.

## 🧪 Testes

A suite tem 39 testes (`cargo test`, executados em Windows e Linux na CI com o DRILLPRO e o ROUTEPRO descarregados junto ao servidor): 28 nos módulos (definições e as suas gamas, utilizadores, papéis, tokens, o abrandamento perante palavras-passe erradas, os ficheiros e o livro de trabalhos, o planeamento), 7 sobre a interface de ponta a ponta através do router real (`tests/api.rs`) e 4 com os programas reais a enviar os seus trabalhos a um servidor real numa porta real, com a máquina no simulador (`tests/programs.rs`). A CI executa também `cargo fmt --all --check`, `cargo clippy` com avisos como erros e `python -I tools/ci_validate.py`. Nada disto foi testado contra um nó ou uma máquina reais.

## 🏗️ Compilação e publicação

A versão vive em `Cargo.toml` e em `eh3d.project.json`, e cada versão tem a sua entrada em [CHANGELOG.md](CHANGELOG.md). `build.sh` / `build.bat` compilam o binário de release (`target/release/eh3d-server`) com um painel em direto, e `test.sh` / `test.bat` executam só os testes; nenhum altera a versão. Ainda não foi publicada nenhuma versão. Numa CM5 (Linux aarch64) compila-se na própria máquina; `docs/DEPLOY.md` instala-o como serviço.

## 📂 Estrutura do repositório

<p align="center">
  <img src="images/repository-structure.svg" alt="Estrutura do repositório EH3D-SERVER" width="100%">
</p>

<details>
<summary>Versão em texto da árvore</summary>

```text
EH3D-SERVER/
├── src/
│   ├── api.rs                                   as rotas REST e o websocket
│   ├── auth.rs                                  utilizadores, papéis, tokens e abrandamento perante palavras-passe erradas
│   ├── plan.rs                                  do pedido aos elementos que a máquina executa
│   ├── jobs.rs state.rs                         os trabalhos e aquilo de que o servidor é feito enquanto corre
│   ├── machines.rs store.rs                     as máquinas e os ficheiros escritos através de um temporário
│   └── config.rs lib.rs main.rs                 o ficheiro de definições, a raiz da biblioteca e o programa
├── tests/                                       a interface de ponta a ponta no simulador e os programas reais contra um servidor real
├── docs/                                        a API, o guia para executar o servidor e os dados das duas imagens
├── deploy/                                      a unidade systemd do serviço
├── server.cfg.example                           o ficheiro de definições com cada chave explicada
├── tools/                                       verificação da CI e do README, o painel dos scripts e ferramentas de imagens
├── images/                                      banner e as imagens de Arquitetura e Estrutura do repositório
├── .github/                                     fluxos de CI e de publicação, modelos de issues e pull requests
├── build.sh build.bat                           compila o binário de release com um painel em direto
├── run.sh run.bat                               arranca o servidor (compila antes se for preciso)
├── eh3d.project.json                            manifesto: versão, papel, maturidade e comando de compilação
├── Cargo.toml Cargo.lock                        o pacote Rust, as suas dependências e as bibliotecas dos dois programas
├── CHANGELOG.md                                 o que mudou em cada versão
├── SECURITY.md SUPPORT.md                       como comunicar uma vulnerabilidade e onde perguntar
└── CONTRIBUTING.md CODE_OF_CONDUCT.md LICENSE   como contribuir, o código de conduta e a licença
```

</details>

## 🗺️ Roteiro e limites conhecidos

**Limites conhecidos hoje**

* Nada correu numa máquina real através do servidor: os trabalhos só correram contra o simulador e a ligação ao EH3D-NODE-BUNGARD não foi alcançada a sério.
* O servidor não termina TLS; ponha HTTPS à frente antes que algo de fora lhe chegue. Não teve revisão independente.
* Os clientes que o usarão além dos programas (a app Android, o Studio, o painel tátil) ainda não existem.
* Precisa das pastas do EH3D-DRILLPRO e do EH3D-ROUTEPRO ao lado, porque usa as suas bibliotecas.
* Um trabalho que corria quando o servidor parou é marcado como falhado e não retoma.

**A seguir**

* Executar um trabalho completo numa máquina real através do servidor e do nó.
* HTTPS no próprio servidor.
* Usar os LED do nó e o seu buffer para terminar um trabalho retido quando o servidor desaparece.
* Construir o primeiro cliente além dos programas.

## 🌍 Idiomas

O README e os textos existem em inglês, espanhol, francês, italiano, alemão, chinês, japonês e português europeu.

## 🔗 Projetos relacionados

* [EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO): o programa de furação; a sua biblioteca planeia e executa os trabalhos de furação
* [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO): o programa de fresagem; a sua biblioteca planeia os trabalhos de fresagem
* [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD): o nó na caixa da máquina com que o servidor fala
* [EH3D-COMMON](https://github.com/JuanenRac/EH3D-COMMON): as partes comuns da família

## 📚 Documentação

* [A API](docs/API.md): cada pedido, o trabalho e a vista que um cliente desenha, os estados e o websocket.
* [Executar o servidor](docs/DEPLOY.md): a compilação, o primeiro arranque, o serviço na CM5 e a segurança da instalação.
* [Registo de alterações](CHANGELOG.md)

## 🤝 Contribuir e comunidade

* Antes de um pull request execute `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` e `python -I tools/ci_validate.py`: tudo tem de passar. Consulte [CONTRIBUTING.md](CONTRIBUTING.md) e o [código de conduta](CODE_OF_CONDUCT.md).
* O README existe em oito idiomas: uma alteração ao texto faz-se nos oito ficheiros, e as duas imagens são desenhadas a partir de `docs/architecture.json` e `docs/structure.json` com `tools/make_architecture.py` e `tools/make_structure.py`.
* Uma alteração a uma rota da API precisa da sua entrada em `docs/API.md`, de um teste através do router real e, para um pedido que altera algo, do papel que o pode fazer.
* Comunique uma vulnerabilidade em privado: [SECURITY.md](SECURITY.md). Perguntas, ideias e relatórios: electrohobby3d@gmail.com ([SUPPORT.md](SUPPORT.md)).

## 👤 AUTOR

**JuanenRac (Electro Hobby 3D)** · electrohobby3d@gmail.com

## 📜 LICENÇA

Software: GPL-3.0-or-later. Ver [LICENSE](LICENSE). Os projetos de hardware da suite usam a CERN OHL-S.
