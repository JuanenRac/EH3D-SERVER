<p align="center">
  <img src="images/EH3D_BANNER.svg" alt="EH3D-SERVER banner" width="100%">
</p>

# 🧭 EH3D-SERVER

<p align="center"><a href="README.md">🇺🇸 English</a> | 🇪🇸 <b>Español</b> | <a href="README_fra.md">🇫🇷 Français</a> | <a href="README_ita.md">🇮🇹 Italiano</a> | <a href="README_deu.md">🇩🇪 Deutsch</a> | <a href="README_zho.md">🇨🇳 中文</a> | <a href="README_jpn.md">🇯🇵 日本語</a> | <a href="README_por.md">🇵🇹 Português</a></p>

### El centro de los programas EH3D: recibe trabajos de DRILLPRO, ROUTEPRO y de los clientes futuros, los planifica, los ejecuta en la máquina a través de su nodo y cuenta a todos cómo van

<p align="center">
  <img src="https://img.shields.io/badge/License-GPL%203.0--or--later-blue.svg" alt="GPL 3.0 or later">
  <img src="https://img.shields.io/badge/Stack-Rust-DEA584.svg" alt="Rust">
  <img src="https://img.shields.io/badge/Languages-8-00E5FF.svg" alt="Languages">
  <img src="https://img.shields.io/badge/Version-0.0.1-0f6cbd.svg" alt="Version">
  <img src="https://img.shields.io/badge/Maturity-scaffolding-FFB020.svg" alt="Maturity">
</p>

---

**Comprobación de honestidad - lo que se ejecuta hoy:** un servidor en Rust (axum) con interfaz REST y websocket que guarda usuarios, máquinas y trabajos, planifica un trabajo de taladrado o de fresado (agujeros y trayectorias, o un fichero Excellon o HP-GL con su tabla de herramientas) con las bibliotecas de EH3D-DRILLPRO y EH3D-ROUTEPRO, muestra una vista previa y lo ejecuta en una máquina a través de su EH3D-NODE-BUNGARD o en un simulador, con los diálogos del operador, una parada y cada cambio contado según ocurre. 55 pruebas se ejecutan en un ordenador, los trabajos contra el simulador. **Todavía no se ha ejecutado nada en una máquina real a través de él**, no termina TLS y de los clientes además de los programas, EH3D-STUDIO existe (nunca probado con una máquina real) y la app de Android y el panel junto a la máquina todavía no existen.

---

> **Estado de seguridad (safety) y de seguridad informática:** esto no es un sistema de seguridad (safety). Un trabajo nunca arranca solo: lo arranca una persona con el rol de operador, un trabajo cada vez en una máquina, y la planificación rechaza agujeros y trayectorias fuera de la mesa y herramientas que faltan o están fuera de rango. Cada petición necesita un token (un observador lee, un operador ejecuta trabajos, un administrador gestiona usuarios, tokens y máquinas); las contraseñas se guardan como hashes Argon2, los tokens de sesión y de programa solo como SHA-256, y la API nunca muestra el token de un nodo. El servidor es para la casa o una VPN hacia ella: pon HTTPS delante antes de que le llegue nada de fuera. La parada de la máquina no depende solo del servidor: el nodo se detiene solo cuando su enlace calla. El proyecto es independiente: no está hecho, avalado ni respaldado por Bungard Elektronik.

## 📋 De un vistazo

| | |
|---|---|
| Versión | `0.0.1` |
| Madurez | Andamiaje |
| Pruebas | 55 |
| Pila tecnológica | Rust (axum, tokio) |
| En hardware real | No: probado contra el simulador; todavía no se ha ejecutado nada en una máquina real a través del servidor |

**Ir a:** [Resumen](#-resumen) · [Arquitectura](#-arquitectura) · [Inicio rápido](#-inicio-rápido) · [Pruebas](#-pruebas) · [Estructura](#-estructura-del-repositorio) · [Relacionados](#-proyectos-relacionados)

## 🚦 Estado del proyecto

Cada línea es una parte de este repositorio y la luz dice hasta dónde llega hoy. Se escribe con lo que el repositorio puede demostrar (pruebas, compilaciones, ejecuciones), no con lo previsto.

🟢 Hecho y verificado · 🟡 Funciona, con límites conocidos · 🟠 Hecho a medias o sin verificar · 🔴 Sin hacer o sin funcionar

| Área | Estado | Qué significa aquí |
|---|---|---|
| Software | 🟡 | Un servidor en Rust (axum) guarda usuarios, máquinas y trabajos, planifica trabajos de taladrado y fresado con las bibliotecas de EH3D-DRILLPRO y EH3D-ROUTEPRO, muestra una vista previa y ejecuta el trabajo con diálogos de operador y una parada. |
| Pruebas | 🟢 | Pasan 55 pruebas (`cargo test`): 38 en los módulos y 17 de la interfaz de extremo a extremo, con trabajos en el simulador (12 en `tests/api.rs`, 4 con los programas reales en `tests/programs.rs`). |
| Interfaz para clientes (API) | 🟡 | Las peticiones REST y los eventos del websocket están escritos en `docs/API.md` y cubiertos por las pruebas; hasta ahora solo los usa la conexión al servidor de los propios programas. |
| Clientes | 🔴 | EH3D-STUDIO lo usa (nunca probado con una máquina real); la app de Android y el panel táctil junto a la máquina todavía no existen. |
| Seguridad | 🟡 | Cada petición necesita un token con uno de tres roles, las contraseñas son hashes Argon2 y las contraseñas erróneas se ralentizan; el servidor no termina TLS (pon HTTPS delante) y no ha tenido revisión independiente. |
| Hardware real | 🟠 | Los trabajos solo se han ejecutado contra el simulador: nada se ha ejecutado en una máquina real a través del servidor. |
| Despliegue | 🟠 | Una unidad de systemd y `docs/DEPLOY.md` describen el servicio en la CM5; este repositorio no registra ningún despliegue como hecho. |
| Integración | 🟡 | Planifica con las mismas bibliotecas que EH3D-DRILLPRO y EH3D-ROUTEPRO (sus carpetas deben estar junto a esta) y habla con el enlace de EH3D-NODE-BUNGARD, al que solo ha llegado a través del simulador. |
| Documentación | 🟢 | README en ocho idiomas, la referencia de la API y la guía para ejecutarlo como servicio. |

## 🎯 Resumen

* Los **trabajos**: un trabajo de taladrado (agujeros y herramientas, o un fichero `.NCD` con su tabla de herramientas) o de fresado (trayectorias y herramientas, o un fichero `.PLT` con su tabla de herramientas), planificado con las bibliotecas de los programas, de modo que el cliente dibuja y la máquina hace exactamente lo mismo; se rechaza con un código cuando un agujero queda fuera de la mesa o falta una herramienta.
* La **vista previa**: el trabajo en el orden en que lo hace la máquina, las herramientas con su número, la caja envolvente, una estimación del tiempo y el elemento en el que termina cada agujero: lo que una pantalla necesita para dibujar la placa y seguir el trabajo.
* La **ejecución**: un trabajo está listo hasta que una persona lo arranca; corre sobre la biblioteca de los programas (el saludo, el armado, la referencia, los agujeros, la parada), espera en los diálogos del operador (cambiar la herramienta, confirmar el husillo) hasta que alguien confirma, y se puede parar; la máquina se detiene con los drivers apagados.
* Las **máquinas**: de cada una, cómo llegar a ella (un EH3D-NODE-BUNGARD por la red, o el simulador), el texto de sus ficheros de configuración y los valores ajustados en ella, de modo que la calibración de una máquina vive en un solo sitio para todos los clientes.
* La **flota y las estaciones**: cada máquina pertenece a una estación (el banco, la sala, el taller) y `GET /api/v1/fleet` responde por todas a la vez, agrupadas por estación, con lo que hace cada una (libre, ocupada, esperando al operador), su trabajo con el progreso y el número de trabajos listos; las máquinas trabajan a la vez, un trabajo en cada una.
* Las **personas y los programas**: usuarios con tres roles, tokens para programas que se muestran una vez, una ralentización ante contraseñas erróneas y un código de puesta en marcha en la consola para el primer administrador.
* Los **eventos**: un websocket que envía cada cambio de un trabajo según ocurre.
* El **servicio**: un binario y un pequeño fichero de texto de ajustes, en el puerto 18200 por defecto (no el de ARMOR ni el de HYDRA-UMC), con una unidad de systemd para la CM5.
* **Previsto, aún sin construir**: los clientes (la app de Android, el Studio, el panel con pantalla táctil junto a la máquina), HTTPS en el propio servidor y el uso de los LED del nodo y su búfer para terminar un trabajo retenido cuando el servidor desaparece.

## 🧩 Papel en EH3D

EH3D-SERVER es el centro de la familia EH3D: los programas le envían sus trabajos, habla con las máquinas a través de sus nodos y los demás clientes ven y envían los mismos trabajos por la misma interfaz. Usa las bibliotecas de [EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO) y [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO) y habla con [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD).

## 🏛️ Arquitectura

<p align="center">
  <img src="images/architecture.svg" alt="Arquitectura de EH3D-SERVER: quién lo usa, qué contiene y de qué depende" width="100%">
</p>

Cómo encajan las partes de este repositorio: quién lo usa, qué contiene y de qué depende. La imagen se dibuja desde `docs/architecture.json` y se comprueba contra el código.

## ✨ Funciones

* **Una vista previa que es el plan:** la vista previa y la ejecución real salen de la misma planificación, así que lo que dibuja el cliente es lo que hace la máquina.
* **Un trabajo nunca arranca solo:** se crea listo y solo lo arranca un operador; un trabajo cada vez en una máquina (`409 machine_busy`).
* **Diálogos del operador por la red:** el trabajo se detiene en el cambio de herramienta y en la confirmación del husillo hasta que un cliente confirma.
* **Eventos en vivo:** un websocket cuenta cada cambio de un trabajo según ocurre, con el resumen de los trabajos al conectar.
* **Calibración en un solo sitio:** los ficheros de configuración y los valores ajustados de cada máquina se guardan en el servidor, así que todos los clientes planifican con los mismos números.
* **Un trabajo que corría cuando el servidor se detuvo se marca como fallido:** nadie sabe dónde estaba la máquina, así que nada se reanuda solo.
* **Ficheros seguros:** usuarios, tokens, máquinas y trabajos son ficheros JSON escritos a través de uno temporal; un fichero dañado es un error, nunca un reinicio silencioso desde cero.
* **Actualizaciones desde GitHub:** con `updates_dir` el servidor ofrece a un administrador la comprobación de las últimas versiones de sí mismo y de EH3D-STUDIO y su actualización (`/api/v1/updates`); un pequeño ayudante lanzado por systemd (`deploy/eh3d-update.py`) descarga, comprueba el SHA-256 de cada fichero, rechaza un archivo con nombres fuera de su carpeta, sustituye la carpeta instalada (la anterior se conserva como `.previous`) y reinicia el servicio. Se ha probado con GitHub y systemd sustituidos por simulaciones, nunca en una instalación real.
* **El nodo del cabezal de taladro a través del servidor:** cuando la configuración de una máquina nombra el nodo (`head_address`, `head_token`), el servidor transmite su estado (`GET /api/v1/machines/:id/head/status`), su cámara como MJPEG (`.../head/stream`, con el token en la dirección para un navegador) y una medida de la mesa (`POST .../head/measure`, operador), así un programa de otra red llega a él con el mismo acceso. Probado con un nodo simulado, nunca con uno real.
* **La verificación de calidad con la cámara del cabezal:** el servidor enciende el aro de LEDs, toma una foto del nodo, la juzga con el analizador del taller (el perfil de cada máquina lo guarda el servidor), hace que el aro diga cómo fue (verde, o rojo parpadeando) y guarda la última verificación para que la pantalla junto a la máquina y los móviles la muestren: `GET/POST .../head/light`, `GET .../head/snapshot`, `POST/GET .../head/inspect`, `GET/PUT .../head/inspect/profile` y `POST .../head/inspect/calibrate`.

## 🔒 Seguridad y protección

| Amenaza | Defensa |
|---|---|
| Cualquiera en la red enviando peticiones | cada petición salvo la comprobación de salud, la primera puesta en marcha y el inicio de sesión necesita un token (`Authorization: Bearer`, o `?token=` para el websocket); tres roles: el observador lee, el operador ejecuta trabajos, el administrador gestiona usuarios, tokens y máquinas |
| Adivinar una contraseña | cinco contraseñas erróneas bloquean el nombre, 30 s y luego 60 s, 120 s y así mientras sigan (`429 throttled`); la respuesta no dice qué parte falló, y un nombre desconocido cuesta lo mismo que uno conocido |
| Una copia de la carpeta de datos | las contraseñas son hashes Argon2id; los tokens de sesión y de programa son aleatorios y solo se guarda su SHA-256, así que una copia de la carpeta no inicia sesión a nadie; la API nunca muestra el token de un nodo |
| Apoderarse de un servidor recién instalado | el primer administrador se crea con un código de puesta en marcha que solo imprime la consola |
| Dejar fuera a todos | el último administrador no se puede borrar, desactivar ni degradar (`409 last_admin`) |
| Un trabajo que arranca solo o un segundo trabajo en una máquina | un trabajo se crea listo; solo un operador lo arranca, y una máquina ejecuta un trabajo cada vez |
| Un plan que chocaría con la mesa o usaría una herramienta que falta | la planificación rechaza con un código agujeros y trayectorias fuera de la mesa y herramientas que faltan o están fuera de rango |
| El servidor muere mientras la máquina se mueve | el nodo se detiene solo cuando su enlace calla; el trabajo se marca como fallido y no se reanuda solo; el servicio detiene sus trabajos con SIGINT |
| Una petición enorme | el cuerpo está limitado (`max_body_mb`, 8 MB por defecto) |
| Un fichero de datos dañado | un error, nunca un arranque silencioso desde cero |
| Un servicio con demasiados derechos | la unidad de systemd corre con su propio usuario, con `NoNewPrivileges`, `ProtectSystem=strict`, `PrivateTmp` y una sola carpeta escribible |

**Lo que no se defiende:** el servidor no termina TLS, así que los tokens y las contraseñas cruzan la red en claro salvo que se ponga HTTPS delante; el token del websocket va en la dirección; no ha tenido revisión independiente. Informa de una vulnerabilidad en privado (véase [SECURITY.md](SECURITY.md)).

## 🔌 Interfaces

| Interfaz | Qué es | Dónde |
|---|---|---|
| REST | `/api/v1/`: `health`, `setup`, `login`, `logout`, `session`, `account`, `users`, `tokens`, `updates`, `machines` (con `preview` y `jobs`), `jobs` (con `start`, `confirm`, `cancel`); JSON de entrada y de salida, errores como `{"error","detail"}` | [docs/API.md](docs/API.md), `src/api.rs` |
| Websocket | `GET /api/v1/events?token=...`: un `hello` con el resumen de los trabajos y luego `job` y `job_removed` en cada cambio | [docs/API.md](docs/API.md) |
| Hacia la máquina | la `connection` de una máquina es `node` (un EH3D-NODE-BUNGARD en una dirección con su token) o `demo` (el simulador) | [docs/API.md](docs/API.md), `src/machines.rs` |
| Línea de comandos | `eh3d-server [--config FICHERO]`; `--help` lo lista | `src/main.rs` |
| Servicio | un binario, puerto 18200 por defecto, una unidad de systemd | [docs/DEPLOY.md](docs/DEPLOY.md), `deploy/eh3d-server.service` |

## ⚙️ Configuración

`server.cfg` (o el fichero dado con `--config`) tiene cinco claves opcionales; una clave desconocida o un valor fuera de rango es un error al arrancar. `server.cfg.example` las explica.

| Clave | Por defecto | Rango | Qué hace |
|---|---|---|---|
| `listen` | `0.0.0.0:18200` | una dirección y un puerto | dónde escucha el servidor (18200 es su propio puerto) |
| `data_dir` | `data` | una carpeta | dónde se guardan usuarios, tokens, máquinas y trabajos; haz copia de seguridad |
| `session_hours` | 12 | de 1 a 720 | cuánto dura un inicio de sesión |
| `max_body_mb` | 8 | de 1 a 256 | la petición más grande, en megabytes |
| `keep_jobs` | 200 | de 10 a 100 000 | cuántos trabajos terminados conserva la lista |

Nombres de usuarios y tokens: letras, dígitos, `.`, `_` y `-`, hasta 32 caracteres; contraseñas: de 8 a 128 caracteres. Los valores ajustados en cada máquina no están aquí: viven en el registro de la máquina, en el formato del fichero de máquina de los programas.

## 🚀 Inicio rápido

Necesitas una cadena de herramientas estable de Rust, y EH3D-DRILLPRO y EH3D-ROUTEPRO junto a esta carpeta (el servidor usa sus bibliotecas).

```bash
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
python -I tools/ci_validate.py
```

Ejecútalo con `run.bat` (Windows) o `./run.sh` (Linux); compila primero si hace falta. Mientras no tiene usuarios, la consola muestra un código de puesta en marcha: crea el administrador con `POST /api/v1/setup`, inicia sesión, añade la máquina con la dirección y el token de su nodo y crea un token para cada programa. `server.cfg.example` lista los ajustes; `docs/DEPLOY.md` lo instala como servicio en la CM5.

## 🧪 Pruebas

La suite tiene 55 pruebas (`cargo test`, ejecutadas en Windows y Linux en el CI con COMMON, DRILLPRO y ROUTEPRO descargados junto al servidor): 38 en los módulos (ajustes y sus rangos, usuarios, roles, tokens, la ralentización ante contraseñas erróneas, los ficheros y el libro de trabajos, la planificación), 12 de la interfaz de extremo a extremo a través del router real (`tests/api.rs`) y 5 con los programas reales enviando sus trabajos a un servidor real en un puerto real, con la máquina en el simulador (`tests/programs.rs`). El CI también ejecuta `cargo fmt --all --check`, `cargo clippy` con advertencias como errores y `python -I tools/ci_validate.py`. Nada de esto se ha probado contra un nodo o una máquina reales. El ayudante que hace las actualizaciones tiene 10 pruebas más propias (`python -I tests/test_update_helper.py`, con GitHub y systemd sustituidos por simulaciones).

## 🏗️ Compilación y publicación

La versión vive en `Cargo.toml` y en `eh3d.project.json`, y cada versión tiene su entrada en [CHANGELOG.md](CHANGELOG.md). `build.sh` / `build.bat` compilan el binario de release (`target/release/eh3d-server`) con un panel en vivo, y `test.sh` / `test.bat` ejecutan solo las pruebas; ninguno cambia la versión. Todavía no se ha publicado ninguna versión. En una CM5 (Linux aarch64) se compila en la propia máquina; `docs/DEPLOY.md` lo instala como servicio.

## 📂 Estructura del repositorio

<p align="center">
  <img src="images/repository-structure.svg" alt="Estructura del repositorio EH3D-SERVER" width="100%">
</p>

<details>
<summary>Versión en texto del árbol</summary>

```text
EH3D-SERVER/
├── src/
│   ├── api.rs                                   las rutas REST y el websocket
│   ├── auth.rs                                  usuarios, roles, tokens y la ralentización ante contraseñas erróneas
│   ├── plan.rs                                  de la petición a los elementos que ejecuta la máquina
│   ├── jobs.rs state.rs                         los trabajos y de qué se compone el servidor mientras corre
│   ├── machines.rs store.rs                     las máquinas y los ficheros escritos a través de uno temporal
│   ├── config.rs lib.rs main.rs                 el fichero de ajustes, la raíz de la biblioteca y el programa
│   ├── updates.rs  head.rs                      las actualizaciones de los servicios
│   └── inspect.rs                               la verificación de calidad con la cámara del cabezal
├── tests/                                       la interfaz de extremo a extremo en el simulador y los programas reales contra un servidor real
├── docs/                                        la API, la guía para ejecutar el servidor y los datos de las dos imágenes
├── deploy/                                      la unidad de systemd del servicio
├── server.cfg.example                           el fichero de ajustes con cada clave explicada
├── tools/                                       comprobación del CI y del README, el panel de los scripts y herramientas de imágenes
├── images/                                      banner y las imágenes de Arquitectura y Estructura del repositorio
├── .github/                                     flujos de CI y de publicación, plantillas de issues y pull requests
├── build.sh build.bat                           compila el binario de release con un panel en vivo
├── run.sh run.bat                               arranca el servidor (compila antes si hace falta)
├── eh3d.project.json                            manifiesto: versión, rol, madurez y comando de compilación
├── Cargo.toml Cargo.lock                        el paquete de Rust, sus dependencias y las bibliotecas de los dos programas
├── CHANGELOG.md                                 qué cambió en cada versión
├── SECURITY.md SUPPORT.md                       cómo informar de una vulnerabilidad y dónde preguntar
└── CONTRIBUTING.md CODE_OF_CONDUCT.md LICENSE   cómo contribuir, el código de conducta y la licencia
```

</details>

## 🗺️ Hoja de ruta y límites conocidos

**Límites conocidos hoy**

* Nada se ha ejecutado en una máquina real a través del servidor: los trabajos solo han corrido contra el simulador y el enlace con EH3D-NODE-BUNGARD no se ha alcanzado de verdad.
* El servidor no termina TLS; pon HTTPS delante antes de que le llegue nada de fuera. No ha tenido revisión independiente.
* De los clientes además de los programas, EH3D-STUDIO existe; la app de Android y el panel táctil todavía no existen.
* Necesita las carpetas de EH3D-DRILLPRO y EH3D-ROUTEPRO a su lado, porque usa sus bibliotecas.
* Un trabajo que corría cuando el servidor se detuvo se marca como fallido y no se reanuda.

**Siguiente**

* Ejecutar un trabajo completo en una máquina real a través del servidor y del nodo.
* HTTPS en el propio servidor.
* Usar los LED del nodo y su búfer para terminar un trabajo retenido cuando el servidor desaparece.
* Construir el primer cliente además de los programas.

## 🌍 Idiomas

El README y los textos existen en inglés, español, francés, italiano, alemán, chino, japonés y portugués de Portugal.

## 🔗 Proyectos relacionados

* [EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO): el programa de taladrado; su biblioteca planifica y ejecuta los trabajos de taladrado
* [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO): el programa de fresado; su biblioteca planifica los trabajos de fresado
* [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD): el nodo de la caja de la máquina con el que habla el servidor
* [EH3D-COMMON](https://github.com/JuanenRac/EH3D-COMMON): las partes comunes de la familia

## 📚 Documentación

* [La API](docs/API.md): cada petición, el trabajo y la vista que dibuja un cliente, los estados y el websocket.
* [Ejecutar el servidor](docs/DEPLOY.md): la compilación, el primer arranque, el servicio en la CM5 y la seguridad del despliegue.
* [Registro de cambios](CHANGELOG.md)

## 🤝 Contribuir y comunidad

* Antes de una pull request ejecuta `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` y `python -I tools/ci_validate.py`: todo debe pasar. Consulta [CONTRIBUTING.md](CONTRIBUTING.md) y el [código de conducta](CODE_OF_CONDUCT.md).
* El README existe en ocho idiomas: un cambio en el texto se hace en los ocho archivos, y las dos imágenes se dibujan a partir de `docs/architecture.json` y `docs/structure.json` con `tools/make_architecture.py` y `tools/make_structure.py`.
* Un cambio en una ruta de la API necesita su entrada en `docs/API.md`, una prueba a través del router real y, para una petición que cambia algo, el rol que puede hacerlo.
* Informa de una vulnerabilidad en privado: [SECURITY.md](SECURITY.md). Preguntas, ideas e informes: electrohobby3d@gmail.com ([SUPPORT.md](SUPPORT.md)).

## 👤 AUTOR

**JuanenRac (Electro Hobby 3D)** · electrohobby3d@gmail.com

## 📜 LICENCIA

Software: GPL-3.0-or-later. Véase [LICENSE](LICENSE). Los diseños de hardware de la suite usan CERN OHL-S.
