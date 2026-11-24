# libre-pedals — Pedalera virtual en Rust

![CI](https://github.com/jimmus9/libre-pedals/actions/workflows/ci.yml/badge.svg)

Motor de efectos de guitarra en tiempo real para Windows escrito en Rust, con interfaz gráfica en cascada, analizador espectral PRE/POST en tiempo real y soporte tanto para **ASIO** (interfaces de audio dedicadas y drivers como FL Studio ASIO / ASIO4ALL) como para **WASAPI** (entrada de línea/micrófono de 3.5 mm o micrófonos USB).

## Estructura del workspace

- `crates/dsp`: Procesamiento de audio puro sin dependencias de hardware (`NoiseGate`, `Overdrive`, `Gain`, `DcBlocker`, `Chain`, `Resampler` cúbico y analizador FFT).
- `crates/engine`: Motor de entrada/salida con `cpal`, ring buffer lock-free (`rtrb`), control de deriva de reloj para WASAPI, parámetros atómicos con soporte para múltiples instancias por pedal, medidores y formato JSON de presets (`Preset`).
- `app` (paquete `pedalera`): Aplicación gráfica (`eframe`/`egui`), gestión de presets de usuario/fábrica, persistencia en `settings.json` y subcomandos CLI (`gui`, `list`, `presets`, `run`).

## Descargas y CI

En cada `push` a `main` y en cada *Pull Request*, GitHub Actions ejecuta `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test --workspace` y genera dos artefactos descargables para Windows (`libre-pedals-wasapi-windows-x64.zip` y `libre-pedals-asio-windows-x64.zip`) en la pestaña **Actions**.

Al crear una etiqueta con prefijo `v*` (por ejemplo `v0.2.0`), el workflow publica automáticamente una **Release** en GitHub con ambos binarios adjuntos.

## Presets y configuración

Los presets guardan el sonido de la cadena de efectos de forma portable entre equipos:

- **Master Volume fuera del preset:** El volumen maestro global permanece intacto al cambiar de preset para evitar saltos bruscos de volumen. Cada preset tiene su propio ajuste `preset_level_db` con rampa suave.
- **Múltiples copias por tipo:** Puedes montar hasta **4 instancias** del mismo tipo de pedal en la misma cadena (máximo 12 pedales en total), reordenarlos libremente y dar clic derecho en el título de un pedal para renombrarlo (por ejemplo, `"Boost"` y `"Drive"`).
- **Presets de fábrica incluidos:** `Clean`, `Crunch` y `Lead (Boost + Drive)`.
- **Ubicación de presets de usuario:** `%APPDATA%\libre-pedals\presets\*.json`.
- **Configuración local separada:** Host, dispositivos de entrada/salida, canal, tamaño de buffer, resolución DSP, Master Volume y último preset se guardan aparte en `%APPDATA%\libre-pedals\settings.json`.

Ejemplo de preset JSON (`format_version: 1`):

```json
{
  "format_version": 1,
  "name": "Lead (Boost + Drive)",
  "author": "libre-pedals",
  "description": "Doble overdrive en cascada: Boost empujando a Drive",
  "chain": [
    {
      "id": "noise_gate-1",
      "pedal": "noise_gate",
      "enabled": true,
      "params": {
        "threshold_db": -55.0
      }
    },
    {
      "id": "overdrive-1",
      "pedal": "overdrive",
      "label": "Boost",
      "enabled": true,
      "params": {
        "drive_db": 4.0,
        "tone_hz": 7000.0,
        "level_db": 6.0
      }
    },
    {
      "id": "overdrive-2",
      "pedal": "overdrive",
      "label": "Drive",
      "enabled": true,
      "params": {
        "drive_db": 26.0,
        "tone_hz": 4200.0,
        "level_db": -14.0
      }
    }
  ],
  "preset_level_db": -2.0
}
```

## Requisitos de compilación en Windows

- **Compilación básica (solo WASAPI):**
  - Rust (`rustup` con toolchain `x86_64-pc-windows-msvc`)
  - Visual Studio 2022 Build Tools (carga de trabajo *Desarrollo para el escritorio con C++*)
- **Compilación con soporte ASIO (`--features asio`):**
  - Todo lo anterior, más **LLVM/Clang** (`libclang.dll`, con `LIBCLANG_PATH=C:\Program Files\LLVM\bin`).
  - SDK ASIO de Steinberg: `asio-sys` lo descarga automáticamente en `%TEMP%\asio_sdk` la primera vez que compilas con `--features asio`, o puedes apuntar manualmente a la carpeta extraída con `CPAL_ASIO_DIR`.

## Cómo compilar

Solo WASAPI (no requiere LLVM ni SDK de Steinberg):

```powershell
cargo build --release
```

Con soporte WASAPI + ASIO:

```powershell
$env:LIBCLANG_PATH = "C:\Program Files\LLVM\bin"
cargo build --release --features asio
```

Si la descarga automática del SDK falla o compilas sin red, descarga el zip desde `https://www.steinberg.net/asiosdk`, extráelo y define `CPAL_ASIO_DIR` antes de compilar:

```powershell
$env:LIBCLANG_PATH = "C:\Program Files\LLVM\bin"
$env:CPAL_ASIO_DIR = "<ruta>\ASIOSDK"
cargo build --release --features asio
```

Ejecutar pruebas unitarias y linter:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Ejemplos de uso

Abrir la interfaz gráfica (barra de presets, rack I/O, cascada de pedales y analizador espectral PRE/POST):

```powershell
.\target\release\pedalera.exe
# o explícitamente:
.\target\release\pedalera.exe gui
```

Listar todos los dispositivos y drivers detectados:

```powershell
.\target\release\pedalera.exe list
```

Listar presets de fábrica y de usuario disponibles:

```powershell
.\target\release\pedalera.exe presets
```

Ejecutar por consola cargando un preset de fábrica, usuario o archivo JSON:

```powershell
.\target\release\pedalera.exe run --host wasapi --preset "Lead (Boost + Drive)" --mute
```

Probar con micrófono USB o entrada de línea en WASAPI (con `--mute` para evitar acoples):

```powershell
.\target\release\pedalera.exe run --host wasapi --input DGM20 --mute
```

Tocar con jack de 3.5 mm usando **Line In** y auriculares en WASAPI:

```powershell
.\target\release\pedalera.exe run --host wasapi --input "Line In" --output "Auriculares" --drive 18 --tone 4200 --gate -55 --volume -6
```

Tocar usando ASIO (mismo driver para entrada y salida):

```powershell
.\target\release\pedalera.exe run --host asio --input "FL Studio ASIO" --output "FL Studio ASIO" --drive 20 --tone 4000 --gate -55 --volume -6
```

### Parámetros útiles de `run`

- `--preset <NOMBRE|RUTA>`: Carga un preset de fábrica, de usuario o archivo `.json` al arrancar.
- `--in-channel <N>`: Canal de entrada base 0 (ej. `0` para entrada 1, `1` para entrada 2 de una interfaz estéreo).
- `--buffer <FRAMES>`: Tamaño de buffer solicitado (ej. `128`, `256`). Si el driver lo rechaza (habitual en WASAPI compartido), pasa automáticamente a `Default`.
- `--latency <FRAMES>`: Tamaño de prellenado del ring buffer (por defecto `2 * buffer` o `512`).
- `--drive`, `--tone`, `--level`, `--gate`, `--volume`, `--bypass-drive`, `--bypass-gate`, `--mute`, `--seconds`.

## Consejos para conectar guitarra por jack 3.5 mm

1. **Usa `Line In` (jack azul) antes que `Microphone` (jack rosa):** la entrada de micrófono aplica preamplificación ruidosa y tiene impedancia más baja. Necesitarás un adaptador pasivo de 6.35 mm (guitarra) a 3.5 mm.
2. **Desactiva las mejoras de audio de Windows:** Panel de control de sonido (`mmsys.cpl`) -> pestaña *Grabar* -> Propiedades del dispositivo -> desactiva "Habilitar mejoras de audio" / "Audio Enhancements".
3. **Baja el Mic Boost a 0 dB** si no tienes más remedio que usar la entrada de micrófono.
4. **Usa auriculares**, nunca altavoces abiertos cerca de la guitarra/micrófono, para evitar retroalimentación (acople).
5. **Evita dispositivos virtuales en la cadena** (Voicemeeter, SteelSeries Sonar): añaden latencia extra y posibles cortes. Selecciona directamente el hardware físico o un driver ASIO real.

## Cómo verificar tus drivers ASIO instalados

Desde PowerShell puedes consultar el registro de Windows:

```powershell
Get-ChildItem HKLM:\SOFTWARE\ASIO
```

Y para comprobar cuáles responden realmente con sus canales y frecuencias de muestreo:

```powershell
.\target\release\pedalera.exe list
```

## Licencia

Este proyecto se distribuye bajo la licencia **GNU General Public License v3.0 o posterior (`GPL-3.0-or-later`)** — consulta [LICENSE](LICENSE).

El ASIO SDK de Steinberg cuenta con licencia dual (incluyendo **GPLv3**) desde octubre de 2025, por lo que los binarios compilados con soporte ASIO (`--features asio`) se distribuyen íntegramente bajo GPLv3.
