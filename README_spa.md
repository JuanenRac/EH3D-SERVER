<p align="center">
  <img src="images/EH3D_BANNER.svg" alt="EH3D-SERVER banner" width="100%">
</p>

# 🧭 EH3D-SERVER

<p align="center"><a href="README.md">🇺🇸 English</a> | 🇪🇸 <b>Español</b> | <a href="README_fra.md">🇫🇷 Français</a> | <a href="README_ita.md">🇮🇹 Italiano</a> | <a href="README_deu.md">🇩🇪 Deutsch</a> | <a href="README_zho.md">🇨🇳 中文</a> | <a href="README_jpn.md">🇯🇵 日本語</a> | <a href="README_por.md">🇵🇹 Português</a></p>

### El centro de los programas EH3D: recibe trabajos de DRILLPRO, ROUTEPRO y los clientes que vendrán, los planifica, los ejecuta en la máquina a través de su nodo y cuenta a todos cómo van

<p align="center">
  <img src="https://img.shields.io/badge/License-GPL%203.0--or--later-blue.svg" alt="GPL 3.0 or later">
  <img src="https://img.shields.io/badge/Stack-Rust-DEA584.svg" alt="Rust">
  <img src="https://img.shields.io/badge/Languages-8-00E5FF.svg" alt="Languages">
  <img src="https://img.shields.io/badge/Version-0.0.1-0f6cbd.svg" alt="Version">
  <img src="https://img.shields.io/badge/Maturity-scaffolding-FFB020.svg" alt="Maturity">
</p>

---

**Comprobación de honestidad - lo que se ejecuta hoy:** un servidor en Rust (axum) con interfaz REST y websocket que guarda usuarios, máquinas y trabajos, planifica un trabajo de taladrado o de fresado (agujeros y trayectorias, o un fichero Excellon o HP-GL con su tabla de herramientas) con las bibliotecas de EH3D-DRILLPRO y EH3D-ROUTEPRO, muestra una vista previa, y lo ejecuta en una máquina a través de su EH3D-NODE-BUNGARD, con los diálogos del operador, una parada y cada cambio contado según ocurre. 39 pruebas se ejecutan en un ordenador, los trabajos contra el simulador. **Todavía no ha corrido nada en una máquina real a través de él**, no termina TLS, y los clientes que lo usarán además de los programas (la app Android, el Studio, el panel junto a la máquina) todavía no existen.

---

> **Estado de seguridad (safety) y de seguridad informática:** esto no es un sistema de seguridad. Un trabajo nunca arranca solo: lo arranca una persona con el rol de operador, de uno en uno en cada máquina, y la planificación rechaza agujeros y trayectorias fuera de la mesa y herramientas que faltan o están fuera de rango. Toda petición necesita un token (un observador lee, un operador ejecuta trabajos, un administrador gestiona usuarios, tokens y máquinas); las contraseñas se guardan como hashes Argon2, los tokens de sesión y de programa solo como SHA-256, y la API nunca muestra el token de un nodo. El servidor es para la casa o una VPN hacia ella: pon HTTPS delante antes de que le llegue nada de fuera. La parada de la máquina no depende solo del servidor: el nodo para por sí mismo cuando su enlace calla. El proyecto es independiente: no lo fabrica, respalda ni da soporte Bungard Elektronik.

## 📋 De un vistazo

| | |
|---|---|
| Versión | `0.0.1` |
| Madurez | Andamiaje |
| Pruebas | 39 |
| Pila tecnológica | Rust |
| En hardware real | No: probado contra el simulador; todavía no ha corrido nada en una máquina real a través del servidor |

## 🚦 Estado del proyecto

Cada línea es una parte de este repositorio y la luz dice hasta dónde llega hoy. Se escribe con lo que el repositorio puede demostrar (pruebas, compilaciones, ejecuciones), no con lo previsto.

🟢 Hecho y verificado · 🟡 Funciona, con límites conocidos · 🟠 Hecho a medias o sin verificar · 🔴 Sin hacer o sin funcionar

| Área | Estado | Qué significa aquí |
|---|---|---|
| Software | 🟡 | Un servidor en Rust (axum) guarda usuarios, máquinas y trabajos, planifica trabajos de taladrado y fresado con las bibliotecas de EH3D-DRILLPRO y EH3D-ROUTEPRO, muestra una vista previa y ejecuta el trabajo con diálogos del operador y una parada. |
| Pruebas | 🟢 | Pasan 39 pruebas (`cargo test`), la interfaz de extremo a extremo con trabajos ejecutándose en el simulador. |
| Interfaz para clientes (API) | 🟡 | Las peticiones REST y los eventos del websocket están descritos en `docs/API.md` y cubiertos por las pruebas; por ahora solo los usa la conexión con el servidor de los propios programas. |
| Clientes | 🔴 | La app de Android, el Studio y el panel táctil junto a la máquina todavía no existen. |
| Seguridad | 🟡 | Toda petición necesita un token con uno de tres roles, las contraseñas son hashes Argon2 y las contraseñas erróneas se ralentizan; el servidor no termina TLS (hay que poner HTTPS delante) y no ha tenido revisión independiente. |
| Hardware real | 🟠 | Los trabajos solo se han ejecutado contra el simulador: nada se ha ejecutado en una máquina real a través del servidor. |
| Despliegue | 🟠 | Una unidad de systemd y `docs/DEPLOY.md` describen el servicio en la CM5; no consta ningún despliegue hecho. |
| Integración | 🟡 | Planifica con las mismas bibliotecas que EH3D-DRILLPRO y EH3D-ROUTEPRO (sus carpetas deben estar junto a esta) y habla con el enlace de EH3D-NODE-BUNGARD, al que solo ha llegado a través del simulador. |
| Documentación | 🟢 | README en ocho idiomas, la referencia de la API y la guía para ejecutarlo como servicio. |

## 🎯 Descripción general

* Los **trabajos**: un trabajo de taladrado (agujeros y herramientas, o un fichero `.NCD` con su tabla de herramientas) o de fresado (trayectorias y herramientas, o un fichero `.PLT` con su tabla de herramientas), planificados con las bibliotecas de los programas, de modo que un cliente dibuja y una máquina hace exactamente lo mismo; rechazados con un código cuando un agujero queda fuera de la mesa o falta una herramienta.
* La **vista previa**: el trabajo en el orden en que lo hace la máquina, las herramientas con su cuenta, el rectángulo que lo contiene, una estimación del tiempo y el paso en el que termina cada agujero - lo que una pantalla necesita para dibujar la placa y seguir el trabajo.
* La **ejecución**: un trabajo está listo hasta que una persona lo arranca; corre sobre la biblioteca de los programas (el saludo, el armado, la referencia, los agujeros, la parada), espera en los diálogos del operador (cambiar la herramienta, confirmar el husillo) hasta que alguien confirma, y se puede parar; la máquina para con los drivers apagados.
* Las **máquinas**: de cada una cómo llegar a ella (un EH3D-NODE-BUNGARD por la red, o el simulador), el texto de sus ficheros de configuración y los valores afinados en ella, así la calibración de una máquina vive en un solo sitio para todos los clientes.
* Las **personas y los programas**: usuarios con tres roles, tokens para programas que se muestran una vez, retardo con las contraseñas erróneas y un código de puesta en marcha en la consola para el primer administrador.
* Los **eventos**: un websocket que envía cada cambio de un trabajo según ocurre.
* El **servicio**: un binario y un pequeño fichero de texto de ajustes, en el puerto 18200 por defecto (no el de ARMOR ni el de HYDRA-UMC), con una unidad de systemd para la CM5.
* **Previsto, aún sin hacer**: los clientes (la app Android, el Studio, el panel con pantalla táctil junto a la máquina), HTTPS en el propio servidor, y el uso de los LED del nodo y de su búfer para terminar un trabajo que tiene cuando el servidor desaparece.

## 🧩 Papel en EH3D

EH3D-SERVER es el centro de la familia EH3D: los programas le envían sus trabajos, él habla con las máquinas a través de sus nodos, y los demás clientes ven y envían los mismos trabajos por la misma interfaz. Usa las bibliotecas de [EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO) y [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO) y habla con [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD).

## 📂 Estructura del repositorio

```text
EH3D-SERVER/
├── src/          el servidor: ajustes, usuarios, máquinas, planificación, trabajos, la interfaz y el websocket
├── tests/        la interfaz de punta a punta, con los trabajos corriendo en el simulador
├── docs/         API.md y DEPLOY.md
├── deploy/       la unidad de systemd
├── tools/        ci_validate.py: comprobaciones del manifiesto, las versiones, el changelog y los README
├── images/       banner
├── server.cfg.example  un ejemplo del fichero de ajustes: cópialo a server.cfg (todas las claves son opcionales)
└── eh3d.project.json
```

## 🛠️ COMPILAR Y EJECUTAR

Necesitas una cadena de herramientas de Rust estable, y EH3D-DRILLPRO y EH3D-ROUTEPRO junto a esta carpeta.

```bash
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
python -I tools/ci_validate.py
```

Ejecútalo con `run.bat` (Windows) o `./run.sh` (Linux); compila antes si hace falta. Mientras no tiene usuarios la consola muestra un código de puesta en marcha: crea el administrador con `POST /api/v1/setup`, inicia sesión, añade la máquina con la dirección y el token de su nodo, y crea un token para cada programa. `server.cfg.example` lista los ajustes; `docs/DEPLOY.md` lo instala como servicio en la CM5.

## 🔗 Proyectos relacionados

* [EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO): el programa de taladrado; su biblioteca planifica y ejecuta los trabajos de taladrado
* [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO): el programa de fresado; su biblioteca planifica los trabajos de fresado
* [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD): el nodo en la caja de la máquina con el que habla el servidor
* [EH3D-COMMON](https://github.com/JuanenRac/EH3D-COMMON): las partes comunes de la familia

## 📚 Documentación y comunidad

* [La API](docs/API.md): cada petición, el trabajo y la vista que dibuja un cliente, los estados y el websocket.
* [Puesta en marcha del servidor](docs/DEPLOY.md): la compilación, el primer arranque, el servicio en la CM5 y la seguridad del despliegue.
* [Registro de cambios](CHANGELOG.md)
* [Cómo contribuir](CONTRIBUTING.md)
* [Política de seguridad](SECURITY.md)
* [Soporte](SUPPORT.md)

## 👤 AUTOR

**JuanenRac (Electro Hobby 3D)** · electrohobby3d@gmail.com

## 📜 LICENCIA

Software: GPL-3.0-or-later. Véase [LICENSE](LICENSE). Los diseños de hardware de la suite usan CERN OHL-S.
