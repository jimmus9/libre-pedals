# Investigación Técnica DSP — `libre-pedals`

Base de conocimiento científica y de ingeniería de audio digital para el diseño, implementación y validación matemática de todos los efectos y del motor de `libre-pedals`.

## Propósito y enfoque metodológico

- **Agnóstico del lenguaje en la teoría:** los algoritmos, ecuaciones y criterios de calidad se fundamentan en literatura primaria de DSP de audio (**DAFx**, **JAES**, **IEEE**, **CCRMA Stanford** y referencias estándar de modelado *Virtual Analog*). La traducción a Rust (`crates/dsp`) y su medición de rendimiento (CPU, latencia, estabilidad en `f32`/`f64`) se derivan de esa base.
- **Validación matemática además de auditiva:** cada decisión debe poder comprobarse mediante métricas objetivas reproducibles (THD, balance de armónicos pares vs. impares, IMD y piso de aliasing) y contrastarse luego en tiempo real en el analizador de la aplicación.
- **Documento vivo (flujo iterativo):** si al investigar un capítulo posterior surge una técnica mejor o una restricción compartida (por ejemplo, al estudiar circuitos con realimentación no lineal o filtros de gabinete), se regresa al capítulo anterior correspondiente, se actualiza el análisis y se deja constancia en el registro de revisiones.

---

## Galería aprobada de pedales

Lista objetivo definida para la pedalera (13 familias de pedales; hasta **4 instancias** de cada uno en cascada):

1. **Noise Gate avanzado** *(reemplaza al `NoiseGate` actual en la implementación)*
2. **Clean Boost / Preamp** *(selector `Op-Amp` / `JFET` + `Tilt` + subsónico y headroom `18 V` verificados)*
3. **Compresor Óptico** *(estilo LA-2A / T4B)*
4. **Limitador Brickwall con Lookahead** *(True-Peak ITU-R BS.1770)*
5. **Soft Clipper / Limitador de Cinta (*Tape Limiter*)**
6. **Overdrive unificado** *(reemplaza al `Overdrive` actual: cubre TS-808, SD-1 asimétrico, Klon con Clean Blend y Bluesbreaker en 6 perillas continuas; permite *stacking* usando múltiples instancias)*
7. **Preamp de Válvula** *(Tríodo 12AX7 en cascada)*
8. **EQ Paramétrico (4 bandas + `Level`)** *(útil pre y post distorsión aprovechando las instancias múltiples; filtros *matched* sin *cramping*)*
9. **Wah / Auto-Wah** *(Envelope Filter)*
10. **Simulador de Gabinete** *(Cabinet IR — Convolución)*
11. **Modulación:** Chorus, Phaser y Tremolo
12. **Delay (3 caracteres clásicos):** Digital (repeticiones fieles), Analógico BBD (oscurecimiento y saturación gradual por repetición) y Tape Delay (con *wow/flutter* y saturación de cinta)
13. **Reverb:** algorítmica (**Plate / Hall**) y **Spring Reverb** *(resorte clásico de amplificador de guitarra)*

---

## Mapa de capítulos

| Capítulo | Archivo | Alcance | Estado |
|---|---|---|---|
| **1. Fundamentos DSP transversales** | [01_fundamentos_dsp.md](./01_fundamentos_dsp.md) | Antialiasing (oversampling HIIR/CIC, ADAA1/ADAA2), filtros TPT/ZDF, Nyquist *cramping*, suavizado continuo y `Stepped`, *denormals* (FTZ/DAZ) y métricas objetivas (THD, pares/impares, IMD, aliasing, ESR). | ✅ Cerrado (`v1.2`) |
| **2. Saturación, Overdrive y Preamps** | [02_saturacion_y_overdrive.md](./02_saturacion_y_overdrive.md) | Clean Boost (`D8`), Overdrive unificado (`D6`), Preamp de válvula 12AX7 en cascada (`D7`), protocolo experimental *White-Box* (WDF/Wright $\omega$) vs. *Gray-Box* (`D5`). | ✅ Cerrado (`v1.2`, experimento `D5` con resultados y gráficas) |
| **3. Dinámica** | [03_dinamica.md](./03_dinamica.md) | Noise Gate avanzado (*sidechain*, histéresis, *hold*, *range*), Compresor Óptico (modelo T4B estilo LA-2A), Limitador Brickwall con *lookahead* ($\mathcal{O}(1)$ + *True-Peak*) y Soft Clipper / Tape Limiter. | ✅ Cerrado e implementado (`v1.2`, Ola 3) |
| **4. Filtros, Gabinete, Modulación y Tiempo** | [04_filtros_cab_modulacion_tiempo.md](./04_filtros_cab_modulacion_tiempo.md) | EQ Paramétrico sin *cramping*, Wah/Auto-Wah, Cabinet IR por convolución particionada de latencia cero (UPOLS), Chorus/Phaser/Tremolo, Delay (Digital/BBD/Tape) y Reverb (Plate/Hall/Spring). | ✅ Cerrado e implementado (`v1.3`, Olas 4 y 5) |

---

## Registro de revisiones y decisiones

| Fecha | Capítulo | Cambio / Decisión | Motivo |
|---|---|---|---|
| 2026-11-24 | Cap. 1 (`v1.1`) | Cierre de decisiones `D1`–`D4` (todas Opción A); partición continua de $M(f_s)$ sobre $(0, \infty)$ ($8\times / 4\times / 2\times / 1\times$); EQ *matched* en todas las $f_s$ sin conmutación a $48\text{ kHz}$; nueva sección 2.5 de análisis de fronteras y test de invarianza entre $f_s$. | Evitar huecos en el rango de muestreo, eliminar discontinuidades de diseño en $48\text{ kHz}$ y verificar formalmente que las combinaciones técnicas no alteren la respuesta ni introduzcan artefactos audibles. |
| 2026-11-24 | Cap. 1 (`v1.2`) | Añadido soporte de selectores discretos `Stepped` con *crossfade* ($5\text{–}10\text{ ms}$) en Sección 2.3 y métrica **ESR** en Sección 3.2. | Evitar clics al conmutar modos en Clean Boost/Compresor/Tape Limiter y contar con métrica temporal rigurosa para comparar modelos *White-Box* vs. *Gray-Box*. |
| 2026-11-24 | Cap. 2 (`v1.1`) | Cierre de decisiones `D5`–`D8`: experimento comparativo `D5` acotado (`16×` NR `f64`), Overdrive único unificado de 6 perillas (`D6`, ajustando galería a 13 familias), Preamp 12AX7 LUT Koren + RC dinámico (`D7`), y Clean Boost de 3 controles + subsónico $30\text{ Hz}$ / *headroom* $18\text{ V}$ fijos (`D8`). | Simplificar controles manteniendo versatilidad musical mediante *stacking* de instancias y dejar acotada la comparativa científica *White-Box* vs. *Gray-Box*. |
| 2026-11-24 | Cap. 3 (`v1.1`) | Cierre de decisiones `D9`–`D12` (todas en Opción A): Noise Gate con histéresis + *sidechain* interno fijo ($120\text{ Hz}\text{–}4.5\text{ kHz}$), Compresor Óptico T4B dual-stage con `Mix`, Limitador Brickwall con *lookahead* $1.5\text{ ms}$ y cola monótona $\mathcal{O}(1)$ + *True-Peak*, y Soft Clipper / Tape Limiter calibrado con `ADAA1`. | Mantener controles limpios en vivo con baja latencia y máxima estabilidad matemática. |
| 2026-11-24 | Cap. 4 (borrador) | Redacción inicial del Capítulo 4 (EQ, Wah, Cabinet IR UPOLS, Modulación, Delay y Reverb) y definición de decisiones abiertas `D13`–`D17`. | Completar la base científica de toda la galería antes de confirmar la investigación en GitHub e iniciar las olas de implementación en código. |
| 2026-11-24 | Cap. 4 (`v1.1`) | Cierre de decisiones `D13`–`D17`: solo **EQ Paramétrico** (`D13` Opción B), **`Wah`** unificado (`D14` A), **`Cabinet IR`** UPOLS con 3 IRs + carga `.wav` (`D15` A), **Modulación** en 3 pedales (`D16` A), y **`Delay`/`Reverb`** multimodo (`D17` A). | Simplificar la galería enfocando la ecualización en el EQ Paramétrico con múltiples instancias y dar por cerrada la base teórica Cap. 1–4 para arrancar la Ola 1. |
| 2026-11-24 | Cap. 2 (`v1.2`) | Ejecución del experimento `D5` en `--release` (`d5_experiment.rs` + `d5_report.rs`), 3 gráficas SVG y selección de **Gray-Box + `ADAA1`** para producción en `D6`. | Confirmar cuantitativamente el balance entre fidelidad física (`ESR`), rechazo de aliasing (`+20.4/+21.7 dB ASNR`) y consumo CPU (`6–8.4×` menor) antes de implementar los pedales de Ola 2. |
| 2026-11-24 | Cap. 3 (`v1.2`) | Implementación completa de **Ola 3 (Dinámica)**: `NoiseGate` avanzado, `OptoCompressor`, `BrickwallLimiter` (*True-Peak* $\mathcal{O}(1)$) y `SoftClipTape`, integrados con 4 instancias en `engine` y preset `"Clean Comp (Opto + Limiter)"`. | Cubrir puerta sin *chatter*, compresión optoelectrónica T4B con memoria, limitación *brickwall* con $0.00\text{ dB}$ de *overshoot* y saturación de cinta/soft-clipping con `ADAA1`. |
| 2026-11-25 | Cap. 4 (`v1.2`) | Implementación de **Ola 4 (Tono y Simulación de Gabinete)** + telemetría de latencia algorítmica del *stack* (`Effect::latency_samples`) y `% CPU` en `engine`/`GUI`, más actualización de hoja de ruta a **8 olas** (incluyendo guía musical interactiva, grabadora `.wav` y plugin DAW `VST3`/`CLAP`). | Completar el bloque tonal y simulación de altavoz con latencia `0`, incorporar carga de `.wav` externos sin dependencias externas y auditar latencia/CPU al cierre de cada ola antes de Modulación/Tiempo y funciones de producto. |
| 2026-11-25 | Cap. 4 (`v1.3`) | Implementación y auditoría de **Ola 5 (Modulación y Tiempo)**: `Chorus`, `Phaser`, `Tremolo` (`Optical`/`Harmonic`), `Delay` (`Digital`/`BBD`/`Tape`), `Reverb` (`Plate`/`Hall`/`Spring`), `MAX_MOUNTED_PEDALS = 16`, 8 presets de fábrica y auditoría de CPU/latencia en `48–192 kHz` (las 15 familias simultáneas). | Completar las 13 familias DSP de la galería (15 pedales contando Modulación separada) con `0` muestras de latencia extra y margen holgado en tiempo real hasta `192 kHz` antes de pasar a la Ola 6 (Guía y GUI). |

---

## Hoja de ruta de implementación (Etapa 2 en código)

Una vez cerradas `D13`–`D17` y versionada la carpeta `docs/research/`, la traducción a Rust (`crates/dsp`, `crates/engine`, `app`) y la evolución hacia producto completo (*Standalone* + Plugin DAW) se ejecutan en **8 olas incrementales verificables**, auditando **latencia algorítmica y `% CPU` del *stack*** al cierre de cada ola:

1. **✅ Ola 1 — Cimientos DSP y Banco de Medición (`crates/dsp` + Analizador):**
   - Implementar bloques transversales: oversampling **HIIR** adaptativo $M(f_s)$, **ADAA1** estable en `f64`, filtros **Cytomic SVF-TPT**, filtros *matched* **Vicanek/Orfanidis**, suavizado exponencial + selectores `Stepped` sin clics y protección **FTZ/DAZ** en el hilo de audio.
   - Integrar el **banco de pruebas offline coherentes** (`THD`, $H_2/H_3$, `IMD`, `ASNR`, `ESR`, test de invarianza $44.1\text{–}192\text{ kHz}$) más el **generador de tonos de prueba** y ventana **Blackman-Harris** + métricas en tiempo real en la GUI.
2. **✅ Ola 2 — Experimento `D5` y Familia de Saturación:**
   - Ejecutar el experimento comparativo acotado **White-Box (WDF + Wright $\omega$) vs. Gray-Box** frente a *Ground Truth* (`16×` NR `f64`) con gráficas y métricas documentadas en el Cap. 2.
   - Implementar **Overdrive unificado (6 perillas)**, **Clean Boost / Preamp** y **Preamp de Válvula 12AX7** en cascada.
3. **✅ Ola 3 — Dinámica:**
   - Reemplazar `NoiseGate` por el **Noise Gate avanzado** (histéresis, *sidechain*, `Range`, `Hold`/`Release`) e implementar **Compresor Óptico (LA-2A)**, **Limitador Brickwall (`1.5 ms` lookahead + True-Peak)** y **Soft Clipper / Tape Limiter**.
4. **✅ Ola 4 — Tono, Simulación de Gabinete (`UPOLS`) y Telemetría de Latencia/CPU:**
   - Implementar **EQ Paramétrico (4 bandas + `Level`)**, **Wah / Auto-Wah** (`Manual / Auto Up / Auto Down`) y **Cabinet IR** por convolución particionada de **latencia cero** (`UPOLS` + 3 IRs libres + carga `.wav` + filtros `Low/High Cut`).
   - Exponer latencia algorítmica del *stack* (`Effect::latency_samples`) y `% CPU` en tiempo real en `engine` y `app`, actualizar presets con gabinete (`Crunch`, `Lead`, `Tube Stack`, `Funk Auto-Wah + 1x12`) y auditar latencia/CPU frente a Ola 3.
5. **✅ Ola 5 — Modulación, Tiempo y Auditoría del Stack Completo:**
   - Implementar **Chorus**, **Phaser**, **Tremolo** (`Óptico / Harmonic`), **Delay (`Digital / BBD / Tape`)** y **Reverb (`Plate / Hall / Spring`)**.
   - Ajustar 8 presets de fábrica (`Clean`, `Crunch`, `Lead`, `80s Chorus Clean`, `Surf Spring & Tremolo`, etc.) y completar la auditoría de latencia y presupuesto CPU a `48–192 kHz` sobre las 15 familias simultáneas (`8.25%` @ `48k`, `10.97%` @ `96k`, `17.77%` @ `192k`).
6. **Ola 6 — Guía Interactiva para Músicos + Rediseño de GUI:**
   - Implementar el nuevo diseño visual de la pedalera sobre la galería definitiva de 13 familias.
   - Incorporar menú contextual / *tooltips* al pasar el mouse y botón `?` con lenguaje práctico para músicos (efecto audible esperado, dirección del giro y ajustes sugeridos).
7. **Ola 7 — Grabadora y Exportación `.wav`:**
   - Captura en tiempo real sin bloqueos mediante ring buffer dedicado y exportación a **`.wav` (`32-bit float` y `24-bit PCM`)**, con opción simultánea de **señal procesada + pista DI limpia** para *re-amping*.
8. **Ola 8 — Arquitectura Dual y Plugin DAW (`VST3` / `CLAP`):**
   - Desacoplar el núcleo de cadena/parámetros/GUI respecto a `cpal`, integrar `nih-plug` con compensación automática de latencia (**PDC** vía `latency_samples`), persistencia de estado en proyectos DAW y empaquetado de artefactos `.vst3` / `.clap` en CI.
