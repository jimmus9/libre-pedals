# Pedalera Virtual en Rust (Prototipo CLI)

Motor de efectos de guitarra en tiempo real para Windows escrito en Rust, compatible tanto con **ASIO** (interfaces de audio dedicadas y drivers genéricos como FL Studio ASIO / ASIO4ALL) como con **WASAPI** (entrada de línea/micrófono de 3.5 mm o micrófonos USB).

## Estructura del workspace

- `crates/dsp`: Procesamiento de audio puro sin dependencias de hardware (`NoiseGate`, `Overdrive`, `Gain`, `DcBlocker`, `Chain`).
- `crates/engine`: Motor de entrada/salida con `cpal`, ring buffer lock-free (`rtrb`), control de deriva de reloj para WASAPI, parámetros atómicos y medidores.
- `app` (paquete `pedalera`): Aplicación de línea de comandos para listar dispositivos y ejecutar la cadena de efectos.

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

Ejecutar pruebas unitarias de DSP:

```powershell
cargo test --workspace
```

## Ejemplos de uso

Listar todos los dispositivos y drivers detectados:

```powershell
.\target\release\pedalera.exe list
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
