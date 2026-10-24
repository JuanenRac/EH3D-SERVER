<p align="center">
  <img src="images/EH3D_BANNER.svg" alt="EH3D-SERVER banner" width="100%">
</p>

# 🧭 EH3D-SERVER

<p align="center"><a href="README.md">🇺🇸 English</a> | <a href="README_spa.md">🇪🇸 Español</a> | <a href="README_fra.md">🇫🇷 Français</a> | <a href="README_ita.md">🇮🇹 Italiano</a> | <a href="README_deu.md">🇩🇪 Deutsch</a> | <a href="README_zho.md">🇨🇳 中文</a> | <a href="README_jpn.md">🇯🇵 日本語</a> | 🇵🇹 <b>Português</b></p>

### O centro dos programas EH3D: recebe trabalhos do DRILLPRO, do ROUTEPRO e dos clientes futuros, planeia-os, executa-os na máquina através do seu nó e diz a todos como vão

<p align="center">
  <img src="https://img.shields.io/badge/License-GPL%203.0--or--later-blue.svg" alt="GPL 3.0 or later">
  <img src="https://img.shields.io/badge/Stack-Rust-DEA584.svg" alt="Rust">
  <img src="https://img.shields.io/badge/Languages-8-00E5FF.svg" alt="Languages">
  <img src="https://img.shields.io/badge/Version-0.0.1-0f6cbd.svg" alt="Version">
  <img src="https://img.shields.io/badge/Maturity-scaffolding-FFB020.svg" alt="Maturity">
</p>

---

**Verificação de honestidade - o que funciona hoje:** um servidor em Rust (axum) com interface REST e websocket que guarda utilizadores, máquinas e trabalhos, planeia um trabalho de furação ou de fresagem (furos e trajetórias, ou um ficheiro Excellon ou HP-GL com a sua tabela de ferramentas) com as bibliotecas do EH3D-DRILLPRO e do EH3D-ROUTEPRO, mostra uma pré-visualização, e executa-o numa máquina através do seu EH3D-NODE-BUNGARD, com os diálogos do operador, uma paragem e cada alteração comunicada à medida que acontece. 39 testes correm num computador, os trabalhos contra o simulador. **Ainda nada correu numa máquina real através dele**, não termina TLS, e os clientes que o usarão além dos programas (a app Android, o Studio, o painel junto à máquina) ainda não existem.

---

> **Estado de segurança (safety) e de segurança informática:** isto não é um sistema de segurança. Um trabalho nunca arranca sozinho: arranca-o uma pessoa com o papel de operador, um de cada vez por máquina, e o planeamento recusa furos e trajetórias fora da mesa e ferramentas em falta ou fora de intervalo. Cada pedido precisa de um token (um observador lê, um operador executa trabalhos, um administrador gere utilizadores, tokens e máquinas); as palavras-passe guardam-se como hashes Argon2, os tokens de sessão e de programa só como SHA-256, e a API nunca mostra o token de um nó. O servidor é para a casa ou uma VPN até ela: ponha HTTPS à frente antes que chegue algo de fora. A paragem da máquina não depende só do servidor: o nó pára por si quando a sua ligação se cala. O projeto é independente: não é fabricado, apoiado nem suportado pela Bungard Elektronik.

## 📋 Num relance

| | |
|---|---|
| Versão | `0.0.1` |
| Maturidade | Andaime |
| Testes | 39 |
| Tecnologias | Rust |
| Em hardware real | Não: testado contra o simulador; ainda nada correu numa máquina real através do servidor |

## 🚦 Estado do projeto

Cada linha é uma parte deste repositório e a luz diz até onde chega hoje. É escrita com o que o repositório consegue demonstrar (testes, compilações, execuções), não com o que está previsto.

🟢 Feito e verificado · 🟡 Funciona, com limites conhecidos · 🟠 Feito a meio ou ainda por verificar · 🔴 Por fazer ou a não funcionar

| Área | Estado | O que significa aqui |
|---|---|---|
| Software | 🟡 | Um servidor Rust (axum) guarda utilizadores, máquinas e trabalhos, planeia trabalhos de furação e fresagem com as bibliotecas do EH3D-DRILLPRO e do EH3D-ROUTEPRO, mostra uma pré-visualização e executa o trabalho com diálogos do operador e uma paragem. |
| Testes | 🟢 | Passam 39 testes (`cargo test`), a interface de ponta a ponta com trabalhos a correr no simulador. |
| Interface para clientes (API) | 🟡 | Os pedidos REST e os eventos do websocket estão descritos em `docs/API.md` e cobertos pelos testes; por agora só a ligação ao servidor dos próprios programas os usa. |
| Clientes | 🔴 | A app Android, o Studio e o painel tátil junto à máquina ainda não existem. |
| Segurança | 🟡 | Cada pedido precisa de um token com um de três papéis, as palavras-passe são hashes Argon2 e as erradas são atrasadas; o servidor não termina TLS (é preciso pôr HTTPS à frente) e não teve revisão independente. |
| Hardware real | 🟠 | Os trabalhos só correram no simulador: nada correu numa máquina real através do servidor. |
| Instalação em serviço | 🟠 | Uma unidade systemd e o `docs/DEPLOY.md` descrevem o serviço no CM5; não há registo de nenhuma instalação feita. |
| Integração | 🟡 | Planeia com as mesmas bibliotecas do EH3D-DRILLPRO e do EH3D-ROUTEPRO (as suas pastas têm de estar ao lado desta) e fala com a ligação do EH3D-NODE-BUNGARD, que só alcançou através do simulador. |
| Documentação | 🟢 | README em oito idiomas, a referência da API e o guia para o executar como serviço. |

## 🎯 Descrição geral

* Os **trabalhos**: um trabalho de furação (furos e ferramentas, ou um ficheiro `.NCD` com a sua tabela de ferramentas) ou de fresagem (trajetórias e ferramentas, ou um ficheiro `.PLT` com a sua tabela de ferramentas), planeados com as bibliotecas dos programas, de modo que um cliente desenha e uma máquina faz exatamente o mesmo; recusados com um código quando um furo fica fora da mesa ou falta uma ferramenta.
* A **pré-visualização**: o trabalho pela ordem em que a máquina o faz, as ferramentas com a sua contagem, o retângulo que o contém, uma estimativa do tempo e o passo em que cada furo termina - o que um ecrã precisa para desenhar a placa e seguir o trabalho.
* A **execução**: um trabalho está pronto até uma pessoa o arrancar; corre sobre a biblioteca dos programas (o cumprimento, o armar, a referência, os furos, a paragem), espera nos diálogos do operador (mudar a ferramenta, confirmar o eixo-árvore) até alguém confirmar, e pode ser parado; a máquina pára com os drivers desligados.
* As **máquinas**: de cada uma como chegar a ela (um EH3D-NODE-BUNGARD pela rede, ou o simulador), o texto dos seus ficheiros de configuração e os valores afinados nela, assim a calibração de uma máquina vive num só sítio para todos os clientes.
* As **pessoas e os programas**: utilizadores com três papéis, tokens para programas mostrados uma só vez, um atraso com as palavras-passe erradas e um código de instalação na consola para o primeiro administrador.
* Os **eventos**: um websocket que envia cada alteração de um trabalho à medida que acontece.
* O **serviço**: um binário e um pequeno ficheiro de texto de definições, na porta 18200 por omissão (não a do ARMOR nem a do HYDRA-UMC), com uma unidade systemd para a CM5.
* **Previsto, ainda por fazer**: os clientes (a app Android, o Studio, o painel tátil junto à máquina), HTTPS no próprio servidor, e o uso dos LED do nó e do seu buffer para terminar um trabalho que tem quando o servidor desaparece.

## 🧩 Papel no EH3D

O EH3D-SERVER é o centro da família EH3D: os programas enviam-lhe os seus trabalhos, ele fala com as máquinas através dos seus nós, e os outros clientes veem e enviam os mesmos trabalhos pela mesma interface. Usa as bibliotecas do [EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO) e do [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO) e fala com o [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD).

## 📂 Estrutura do repositório

```text
EH3D-SERVER/
├── src/          o servidor: definições, utilizadores, máquinas, planeamento, trabalhos, a interface e o websocket
├── tests/        a interface de ponta a ponta, com os trabalhos a correr no simulador
├── docs/         API.md e DEPLOY.md
├── deploy/       a unidade systemd
├── tools/        ci_validate.py: verificações do manifesto, das versões, do changelog e dos README
├── images/       banner
├── server.cfg.example  um exemplo do ficheiro de definições: copie-o para server.cfg (todas as chaves são opcionais)
└── eh3d.project.json
```

## 🛠️ COMPILAR E EXECUTAR

Precisa de uma cadeia de ferramentas Rust estável, e do EH3D-DRILLPRO e do EH3D-ROUTEPRO junto a esta pasta.

```bash
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
python -I tools/ci_validate.py
```

Execute-o com `run.bat` (Windows) ou `./run.sh` (Linux); compila primeiro se for preciso. Enquanto não tem utilizadores, a consola mostra um código de instalação: crie o administrador com `POST /api/v1/setup`, inicie sessão, acrescente a máquina com o endereço e o token do seu nó, e crie um token para cada programa. `server.cfg.example` lista as definições; `docs/DEPLOY.md` instala-o como serviço na CM5.

## 🔗 Projetos relacionados

* [EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO): o programa de furação; a sua biblioteca planeia e executa os trabalhos de furação
* [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO): o programa de fresagem; a sua biblioteca planeia os trabalhos de fresagem
* [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD): o nó na caixa da máquina com que o servidor fala
* [EH3D-COMMON](https://github.com/JuanenRac/EH3D-COMMON): as partes comuns da família

## 📚 Documentação e comunidade

* [A API](docs/API.md): cada pedido, o trabalho e a vista que um cliente desenha, os estados e o websocket.
* [Pôr o servidor a correr](docs/DEPLOY.md): a compilação, o primeiro arranque, o serviço na CM5 e a segurança da instalação.
* [Registo de alterações](CHANGELOG.md)
* [Como contribuir](CONTRIBUTING.md)
* [Política de segurança](SECURITY.md)
* [Suporte](SUPPORT.md)

## 👤 AUTOR

**JuanenRac (Electro Hobby 3D)** · electrohobby3d@gmail.com

## 📜 LICENÇA

Software: GPL-3.0-or-later. Ver [LICENSE](LICENSE). Os projetos de hardware da suite usam a CERN OHL-S.
