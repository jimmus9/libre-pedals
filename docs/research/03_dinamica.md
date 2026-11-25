# Capítulo 3 — Procesadores de Dinámica

> **Estado:** ✅ Cerrado e implementado (`v1.2` — decisiones `D9`–`D12` en Opción A implementadas en Ola 3).  
> **Pedales cubiertos:** Noise Gate avanzado *(reemplazo del actual)* · Compresor Óptico *(estilo LA-2A / celda T4B)* · Limitador *Brickwall* con *Lookahead* y *True-Peak* · Soft Clipper / Limitador de Cinta (*Tape Limiter*).  
> **Base previa aplicada:** filtros *sidechain* Cytomic SVF-TPT, parámetro discreto `Stepped` con *crossfade* ($5\text{–}10\text{ ms}$), acumulación `f64` y protección contra *denormals*.

---

## 1. Fundamentos de procesadores de dinámica

El marco matemático estándar en audio digital para compresores, limitadores y puertas de ruido fue unificado por **Giannoulis, Massberg & Reiss (JAES, 2012)** en *"Digital Dynamic Range Compressor Design—A Tutorial and Analysis"*. Sus principios explican por qué unos procesadores suenan transparentes o musicales mientras otros producen *pumping*, *chatter* o distorsión en graves:

### 1.1 Procesado en dominio logarítmico (dB) y topologías
Separar la señal en **cadena principal de audio** y **cadena lateral de control (*sidechain*)** en dominio de decibelios linealiza las envolventes perceptivas y simplifica el cálculo estático de transferencia:

1. **Conversión de nivel:** $x_G[n] = 20 \log_{10}(\max(|x_{\text{sc}}[n]|, 10^{-6}))$ (o detector RMS cuando se busca energía promedio en lugar de transitorios instantáneos).
2. **Computador estático de ganancia:** calcula el nivel objetivo $y_G$ y la atenuación requerida $g_{\text{dB}} = y_G - x_G$.
3. **Topología *Feed-Forward* vs. *Feed-Back*:**
   - ***Feed-Forward*:** mide la entrada limpia antes de aplicar ganancia. Permite ratios $\infty:1$ (*brickwall*), *lookahead* y cálculo determinista sin lazos de realimentación.
   - ***Feed-Back*:** mide la **salida ya atenuada** (retardo implícito de 1 muestra o resolución directa). Es la topología clásica de diseños ópticos/válvula como el **LA-2A**: al depender de su propia salida, el ratio efectivo se vuelve progresivo y autoregulado (a mayor exceso de entrada, la compresión aumenta de manera orgánica sin llegar a aplastar en seco).

### 1.2 Curva de codo suave (*Soft Knee*) — Ecuación exacta
Dado un umbral $T$ (dBFS), ratio $R \ge 1$ y ancho de codo $W > 0$ (en dB, centrado simétricamente alrededor de $T$), la transferencia continua por tramos de **Giannoulis et al. (2012)** es:

$$y_G = \begin{cases} x_G & 2(x_G - T) < -W \\[6pt] x_G + \left(\dfrac{1}{R} - 1\right)\dfrac{\left(x_G - T + \frac{W}{2}\right)^2}{2W} & 2|x_G - T| \le W \\[10pt] T + \dfrac{x_G - T}{R} & 2(x_G - T) > W \end{cases}$$

Cuando $W \to 0$, converge al codo duro (*hard knee*). Un codo suave ($W \approx 6\text{–}12\text{ dB}$) elimina la discontinuidad de pendiente al entrar en compresión, evitando cambios bruscos de timbre al tocar notas cerca del umbral.

### 1.3 Detectores de envolvente y distorsión por modulación en graves
Un hallazgo clave de **Giannoulis et al. (2012)** concierne al diseño del detector de envolvente con ataque $\tau_A$ y relajación $\tau_R$:
- **Detector ramificado (*Branching*):** conmuta limpiamente entre constante $\tau_A$ cuando la envolvente sube y $\tau_R$ cuando cae.
- **Detector desacoplado (*Decoupled*):** coloca una etapa de pico/ataque en serie con una etapa de relajación; si se diseña de forma ingenua sin compensación, el tiempo real observado durante la caída se convierte en $\approx \tau_A + \tau_R$.
- **Distorsión en cuerdas graves de guitarra:** una cuerda Mi grave suena a $82.41\text{ Hz}$ (semiciclo cada $\approx 6.07\text{ ms}$, periodo completo $\approx 12.13\text{ ms}$). Si la relajación $\tau_R$ de un compresor o limitador es demasiado corta ($< 20\text{ ms}$), la ganancia sube y baja **entre cada cresta de la propia onda sinusoidal**, modulando la forma de onda y creando armónicos impares de distorsión no deseada. Por ello, los limitadores rápidos necesitan *lookahead* o un *hold*/liberación adaptativa de dos etapas.

---

## 2. Análisis por pedal de la galería

### 2.1 Noise Gate avanzado *(reemplaza al `NoiseGate` actual)*

El `NoiseGate` actual del repositorio usa un único umbral con seguidor de envolvente de pico, *hold* fijo y cierre total a $-\infty\text{ dB}$. En guitarra eléctrica real presenta tres debilidades típicas: (1) ruido de red a $50/60\text{ Hz}$ o golpes graves pueden abrir la puerta en falso, (2) cuando el *sustain* de una nota cae rozando el umbral se produce apertura/cierre rápido (*chatter*), y (3) cortar a silencio absoluto se siente antinatural frente a amplis reales.

**Mejoras documentadas en ingeniería de audio:**
1. **Doble umbral con Histéresis ($\Delta H$):**
   La puerta se abre cuando el detector supera $T_{\text{open}}$, pero solo se cierra cuando cae por debajo de $T_{\text{close}} = T_{\text{open}} - \Delta H$ (típicamente $\Delta H \in [2, 12]\text{ dB}$). Elimina por completo el *chatter* durante colas largas de *sustain*.
2. **Filtro *Sidechain* interno (SVF-TPT pasa-banda):**
   El detector escucha la señal filtrada (p. ej. pasa-altos a **$120\text{ Hz}$** + pasa-bajos a **$4.5\text{ kHz}$**) **sin alterar el audio principal**:
   - Rechaza zumbidos graves ($50/60\text{ Hz}$ y golpes en el puente) y reduce silbidos ultrasónicos de pastillas *single-coil*, disparando únicamente con la energía útil de las cuerdas ($120\text{ Hz}\text{–}4.5\text{ kHz}$).
3. **Control de profundidad `Range` (piso de atenuación):**
   En lugar de mutear siempre a $-\infty\text{ dB}$, permite limitar el cierre a un piso configurable (p. ej. $-20\text{ a }-80\text{ dB}$, siendo $\le -80\text{ dB}$ cierre total) para mantener respiración natural antes de *overdrives* de media ganancia.
4. **`Hold` y `Release` musicales + *Lookahead* corto ($1\text{ ms}$):**
   El *lookahead* de apenas $1\text{ ms}$ (imperceptible al tocar) abre la envolvente justo antes del transitorio de púa para no mutilar el ataque inicial.

*(En código, mantendrá `kind = "noise_gate"` y el parámetro `threshold_db` por retrocompatibilidad con los presets existentes Clean, Crunch y Lead.)*

---

### 2.2 Compresor Óptico *(estilo Teletronix LA-2A / celda T4B)*

Estudiado empíricamente por **Moore, Till & Wakefield (JAES)** y en modelado VA de celdas optoelectrónicas (**T4B**): el carácter orgánico y casi "transparente pero firme" del LA-2A en guitarra limpia proviene de la física no lineal con memoria de dos componentes acoplados:

1. **Panel electroluminiscente (EL) en *feed-back* + modo `Compress` / `Limit`:**
   - La señal de salida (atenuada) excita un amplificador lateral que ilumina el panel EL de forma no lineal: en modo **`Compress`** el codo es muy amplio y progresivo ($\sim 3:1\text{–}4:1$ suave al subir nivel); en modo **`Limit`** la curva endurece la relación hacia la zona alta de reducción de ganancia.
2. **Fotorresistor de sulfuro de cadmio (CdS) — Memoria óptica y relajación en 2 etapas:**
   Los fotorresistores CdS presentan atrapamiento de portadores en la banda de valencia que origina el famoso **comportamiento dependiente del programa**:
   - **Ataque:** rápido pero redondeado ($\sim 10\text{ ms}$ promedio), dejando pasar una pequeña parte del chasquido natural de la púa.
   - **Relajación en dos fases:**
     1. **Recuperación inicial rápida:** entre el **$50\%$ y $60\%$** de la reducción de ganancia se recupera en **$\sim 50\text{–}60\text{ ms}$**.
     2. **Cola lenta con memoria de exposición:** el **$40\%$–$50\%$** restante tarda entre **$0.5\text{ s}$ y $> 5\text{ s}$** en volver a $0\text{ dB}$, **y la constante de tiempo se vuelve más lenta cuanto mayor haya sido la duración e intensidad de la señal reciente**.

**Modelo estructural de baja carga en tiempo real:**
Para capturar esta física en CPU constante (sin redes neuronales pesadas ni simuladores MNA completos), acoplamos:
- Un integrador rápido ($\tau_{\text{fast}} \approx 55\text{ ms}$) en paralelo/cascada con un integrador lento ($\tau_{\text{slow}} \approx 600\text{ ms}\text{–}3.5\text{ s}$), cuya constante $\tau_{\text{slow}}$ y nivel base son modulados continuamente por un **acumulador de historial de iluminación** $H[n]$ (un filtro *leaky* muy lento, $\tau_{\text{mem}} \approx 5\text{–}10\text{ s}$).
- Control **`Mix` ($0\dots 100\%$)** para compresión paralela ("New York"): mantiene intactos los transitorios limpios sumados al cuerpo sostenido del opto-compresor.

---

### 2.3 Limitador *Brickwall* con *Lookahead* y *True-Peak* (`ITU-R BS.1770`)

Su objetivo al final de la cadena (justo antes del *Master Volume* o salida hacia DAW) es **garantizar matemáticamente que ningún pico supere el techo `Ceiling`**, sin recortar en onda cuadrada ni generar aliasing.

#### 1) Picos inter-muestra (*Inter-Sample Peaks*, `ITU-R BS.1770`)
Dos muestras consecutivas por debajo de $0\text{ dBFS}$ (p. ej. ambas a $-0.5\text{ dBFS}$ antes de cambiar de signo en alta frecuencia) pueden reconstruir en el DAC o durante el *resampleo* final una onda continua con crestas de hasta **$+3\text{ dB}$ o más** por encima de $0\text{ dBFS}$ (*clipping* inter-muestra). Siguiendo el Anexo 2 de **ITU-R BS.1770**, la detección *True-Peak* estima la envolvente continua mediante *upsampling* $\ge 4\times$:
- Aplicamos la misma regla continua sin huecos del Capítulo 1 sobre la rama de detección: $4\times$ si $f_s \le 48\text{ kHz}$, $2\times$ en $48\text{–}176.4\text{ kHz}$, y $1\times$ directo si $f_s \ge 176.4\text{ kHz}$ (a $192\text{ kHz}$ los *ISPs* en banda audible ya quedan acotados con mínima desviación).

#### 2) Ventana de *Lookahead* en $\mathcal{O}(1)$ mediante Cola Monótona
Para ver un pico antes de que llegue y bajar la ganancia gradualmente sin distorsionar el transitorio, el audio se retrasa $L$ muestras (por ejemplo $1.5\text{ ms}$: $72$ muestras a $48\text{ kHz}$, $288$ muestras a $192\text{ kHz}$) y se calcula el máximo móvil en la ventana $[n, n+L]$:
- **Solución óptima:** una **cola monótona decreciente (*monotonic deque*) prealojada en un buffer circular fijo** (sin reservas dinámicas en el hilo de audio). Garantiza complejidad **$\mathcal{O}(1)$ amortizada por muestra** independientemente de $L$ y de $f_s$.

#### 3) Ataque suave exacto sin *overshoot* ni *pumping*
1. Factor crudo de ganancia en cada instante del *lookahead*:
   $$g_{\text{raw}}[n] = \min\!\left(1.0,\; \frac{C}{P_{\text{tp}}[n]}\right)$$
   donde $C = 10^{\text{Ceiling}_{\text{dB}}/20}$ y $P_{\text{tp}}[n]$ es el máximo móvil *True-Peak*.
2. **Suavizado de ataque sin esquinas:** en lugar de un IIR asintótico (que podría llegar tarde al pico o dejar *overshoot*), aplicar un **filtro de mínimo + media móvil / ventana de Hann de longitud $L$** (o un filtro de ataque simétrico seguido de liberación de dos etapas: rápida para micro-transitorios y lenta para señal sostenida) asegura que en el instante exacto en que el pico abandona la línea de retardo, la ganancia suavizada $g_{\text{smooth}}[n] \le g_{\text{raw}}[n]$, garantizando **$0.00\text{ dB}$ de *overshoot*** y transiciones libres de armónicos agresivos.

---

### 2.4 Soft Clipper / Limitador de Cinta (*Tape Limiter*)

A diferencia del *brickwall* transparente, un **Soft Clipper / Tape Limiter** recorta crestas rápidas de forma **instantánea** (latencia $\approx 0$) convirtiendo picos puntuales de púa en armónicos suaves de bajo orden en lugar de bajar el volumen de toda la señal (evitando *pumping* por completo):

| Modo | Mecanismo matemático / físico | Carácter tonal y armónico | Antialiasing |
|---|---|---|---|
| **Soft Clipper paramétrico ($C^2$)** | Función de transferencia con zona lineal hasta $(C - K)$ y transición polinomial/hiperbólica suave hasta la asíntota exacta en `Ceiling` ($C$) regulada por `Knee` ($K$) | Redondeo limpio y simétrico de crestas; conserva pegada y cuerpo en medios cuando se empuja fuerte | `ADAA1` analítico sobre el *waveshaper* + $M(f_s)$ |
| **Tape Limiter *(Chowdhury, DAFx-19)*** | Inspirado en la histéresis ferromagnética de **Jiles-Atherton** y las pérdidas por autodesmagnetización en alta frecuencia de cinta magnética | Compresión dependiente de la frecuencia y memoria magnética: suaviza puntas ásperas de agudos/púa y añade calidez armónica par/impar equilibrada | `ADAA1` / oversampling $M(f_s)$ |

En **Chowdhury (DAFx-19, *"Real-time Physical Modelling for Analog Tape Machines"*)**, el modelo físico completo de **Jiles-Atherton** resuelve una EDO de magnetización $M(H)$ mediante Runge-Kutta (RK2/RK4) para capturar histéresis exacta. Para tiempo real en guitarra, podemos implementar un modelo calibrado ligero (saturación suave asimétrica + pre/post-énfasis dependiente del nivel en agudos) **o** evaluar en un experimento offline tipo `D5` si el solver Jiles-Atherton RK4 aporta una diferencia audible/matemática apreciable.

---

## 3. Coherencia con Cap. 1 y Cap. 2 (latencia, transiciones y métricas)

1. **Presupuesto de latencia para tocar en vivo:**
   - *Noise Gate avanzado*: $0\text{–}1\text{ ms}$ (solo si activa *lookahead* corto).
   - *Compresor Óptico*: $0\text{ ms}$ (latencia nula en la señal principal).
   - *Soft Clipper / Tape Limiter*: $\approx 0\text{ ms}$ (solo retardo grupal mínimo de HIIR/`ADAA1`).
   - *Limitador Brickwall*: **$1.5\text{ ms}$** fijado por el *lookahead* (sumado a $4\text{–}10\text{ ms}$ del driver ASIO/WASAPI mantiene la respuesta muy por debajo del umbral táctil perceptible de $\sim 12\text{–}15\text{ ms}$). Conviene recordar que si `Gain`/`Ceiling` se coloca al final como protección, $1.5\text{ ms}$ es prácticamente imperceptible.
2. **Conmutación de selectores discretos (`Stepped`):**
   - Los cambios entre `Compress` ↔ `Limit` (Óptico) y `Soft Clip` ↔ `Tape` (Tape Limiter) aplican el ***crossfade* de $5\text{–}10\text{ ms}$** definido en Cap. 1 (`v1.2`).
3. **Protocolo de pruebas de laboratorio en `crates/dsp`:**
   - **Noise Gate:** rampa de amplitud lenta sobre ruido blanco + hum de $60\text{ Hz}$ para verificar histéresis ($0$ *chatter* cerca del umbral), rechazo *sidechain* a $60\text{ Hz}$ y respeto estricto del piso `Range`.
   - **Compresor Óptico:** tren de pulsos IEC/AES + silencio posterior para medir las dos constantes de liberación ($\sim 55\text{ ms}$ inicial + cola larga dependiente de duración previa) y THD baja a $82.41\text{ Hz}$ (sin modulación de onda).
   - **Limitador Brickwall:** barrido con *ISPs* sintéticos ($+3\text{ dBFS}$ inter-muestra) verificando **$\text{peak}_{\text{true}} \le \text{Ceiling}$ ($0.00\text{ dB}$ *overshoot*)** y cola monótona libre de asignaciones dinámicas; invarianza entre $44.1\text{–}192\text{ kHz}$.
   - **Soft Clipper / Tape Limiter:** medir curva entrada-salida hasta `Ceiling`, $\text{ASNR}_{\text{dB}}$ con `ADAA1` y respuesta de compresión en agudos del modo `Tape`.

---

## 4. Bibliografía del Capítulo 3

1. **Giannoulis, D., Massberg, M., & Reiss, J. D. (2012).** *"Digital Dynamic Range Compressor Design—A Tutorial and Analysis"*. *Journal of the Audio Engineering Society (JAES)*, vol. 60, n.º 6, pp. 399–408.
2. **Moore, A., Till, R., & Wakefield, J. (JAES / Univ. of Huddersfield).** Estudios empíricos y perceptuales sobre hardware de compresión clásico (*Teletronix LA-2A* / celda optoelectrónica *T4B*, balística y THD).
3. **International Telecommunication Union (ITU-R BS.1770).** *Algorithms to measure audio programme loudness and true-peak audio level* (Anexo 2: detección *True-Peak* mediante sobremuestreo $\ge 4\times$).
4. **Chowdhury, J. (2019).** *"Real-time Physical Modelling for Analog Tape Machines"*. Proceedings of the 22nd International Conference on Digital Audio Effects (DAFx-19), Birmingham (modelo de histéresis ferromagnética de **Jiles-Atherton**, *ChowTapeModel*).
5. **Literatura estándar de limitación *lookahead* y puertas de ruido en audio profesional:** máximo móvil en $\mathcal{O}(1)$ mediante cola monótona (*monotonic deque*), suavizado FIR sin *overshoot*, histéresis de doble umbral y filtrado *sidechain*.

---

## 5. Decisiones cerradas (`v1.1`)

| ID | Pedal | Opciones consideradas | Decisión final |
|---|---|---|---|
| **D9** | **`Noise Gate avanzado`** | **A)** **4–5 perillas limpias** (`Threshold`, `Hysteresis`, `Hold`, `Release`, `Range`), con filtro *sidechain* ($120\text{ Hz}\text{–}4.5\text{ kHz}$), `Attack` ($\sim 1\text{ ms}$) y *lookahead* ($1\text{ ms}$) calibrados internamente (filosofía `D8`)<br>**B)** Exponer también perillas de *Sidechain HPF/LPF* y *Attack* | ✅ **Opción A** |
| **D10** | **`Compresor Óptico` (LA-2A)** | **A)** Controles clásicos: `Peak Reduction`, `Gain`, selector `Stepped` (`Compress` / `Limit`) y perilla `Mix` (paralela), gobernados por el modelo dual-stage RC + memoria CdS<br>**B)** Añadir además perillas manuales de `Attack` y `Release` | ✅ **Opción A** |
| **D11** | **`Limitador Brickwall`** | **A)** *Lookahead* fijo de **$1.5\text{ ms}$** (mínima latencia en vivo, *True-Peak* adaptativo y $0.00\text{ dB}$ *overshoot*) + 3 perillas (`Drive/Input`, `Ceiling`, `Release`)<br>**B)** Añadir selector de *lookahead* (`1.5 ms` en vivo / `5 ms` mastering) | ✅ **Opción A** |
| **D12** | **`Soft Clipper / Tape Limiter`** | **A)** Pedal único con `Drive`, `Ceiling`, `Knee` y selector `Stepped` (`Soft Clip` / `Tape`) usando en modo `Tape` un modelo ligero calibrado + `ADAA1`<br>**B)** Añadir prueba comparativa offline (estilo `D5`) entre el modo `Tape` ligero y el modelo físico completo de **Jiles-Atherton** (RK4) antes de decidir | ✅ **Opción A** |

---

## 6. Implementación en código (Ola 3)

Los 4 procesadores de dinámica están implementados en `crates/dsp`, prealojados con 4 instancias en `crates/engine` y cubiertos por tests de laboratorio automáticos (`cargo test -p dsp`):

| Pedal | Archivo | Parámetros (`id` / rango / defecto) | Latencia |
|---|---|---|---|
| **Noise Gate avanzado** | [`gate.rs`](../../crates/dsp/src/gate.rs) (`kind = "noise_gate"`) | `threshold_db` (`-100..0`, `-60 dB`), `hysteresis_db` (`0..12`, `6 dB`), `hold_ms` (`0..500`, `50 ms`), `release_ms` (`5..1000` log, `100 ms`), `range_db` (`-80..0`, `-80 dB`) | $1\text{ ms}$ fija (`48` m @ `48 kHz`, `192` m @ `192 kHz`) |
| **Compresor Óptico** | [`opto_comp.rs`](../../crates/dsp/src/opto_comp.rs) (`kind = "opto_comp"`) | `peak_reduction` (`0..100%`, `40%`), `gain_db` (`-6..24`, `4 dB`), `mode` (`Stepped`: `Compress / Limit`), `mix` (`0..100%`, `100%`) | $0$ muestras (*feedback* puro) |
| **Limitador Brickwall** | [`limiter.rs`](../../crates/dsp/src/limiter.rs) (`kind = "limiter"`) | `drive_db` (`0..24`, `0 dB`), `ceiling_db` (`-12..0`, `-1 dB`), `release_ms` (`10..1000` log, `120 ms`) | $L + T$ (`74` @ `44.1k`, `80` @ `48k`, `152` @ `96k`, `288` @ `192k`) |
| **Soft Clip / Tape** | [`tape_limiter.rs`](../../crates/dsp/src/tape_limiter.rs) (`kind = "soft_clipper"`) | `drive_db` (`0..24`, `6 dB`), `ceiling_db` (`-12..0`, `-1 dB`), `knee` (`0..100%`, `50%`), `mode` (`Stepped`: `Soft Clip / Tape`) | Mínima de `Oversampler` HIIR + `ADAA1` |

### Decisiones de ingeniería aplicadas
- **Noise Gate (`gate.rs`):** filtro *sidechain* de 4.º orden `SvfTpt` High-Pass a $120\text{ Hz}$ ($\approx -24.6\text{ dB}$ en $60\text{ Hz}$) más Low-Pass a $4.5\text{ kHz}$; detector de pico ($0.25 / 30\text{ ms}$) con máquina de histéresis y *lookahead* constante de $1\text{ ms}$ incluso con umbral $\le -100\text{ dB}$.
- **Compresor Óptico (`opto_comp.rs`):** topología *feedback* con detector RMS de $30\text{ ms}$ en `f64`, codo suave Giannoulis (`Compress` $R=3, W=12\text{ dB}$; `Limit` $R=12, W=6\text{ dB}$) con `ModeCrossfader` ($8\text{ ms}$), celda CdS dual-stage ($\tau_{\text{att}} = 10\text{ ms}$, $\tau_{\text{fast}} = 55\text{ ms}$ con peso $55\%$, $\tau_{\text{slow}} = 0.6 + 2.9 H\text{ s}$ con peso $45\%$) e integrador de memoria $H$ ($\tau_{\text{carga}} = 3\text{ s}$, $\tau_{\text{descarga}} = 8\text{ s}$).
- **Limitador Brickwall (`limiter.rs`):** detector *True-Peak* FIR polifásico de 16 *taps*/fase (sinc-Hann) bajo partición $M(f_s)$ (`4×` $\le 50\text{ kHz}$, `2×` hasta $176.4\text{ kHz}$, `1×` $\ge 176.4\text{ kHz}$, retardo $T=8$ o $0$), mínimo móvil $\mathcal{O}(1)$ por cola monótona circular de ventana $L+1$ ($L \approx 1.5\text{ ms}$) seguido de media móvil *boxcar* de longitud $L+1$, garantizando $g_{\text{smooth}}[j] \le g_{\text{raw}}[j-L]$ y **$0.00\text{ dB}$ de *overshoot*** sin recortes duros.
- **Soft Clip / Tape (`tape_limiter.rs`):** rama `Soft Clip` $C^2$ continua lineal hasta $C - K$ con asíntota $C$ bajo `ADAA1` y `Oversampler` $M(f_s)$; rama `Tape` con pre-énfasis `MatchedBiquad` High-Shelf ($3\text{ kHz}$, $+2\dots+8\text{ dB}$ según `Knee`), `AsymSoftClipAdaa1` ($C$ vs $0.92 C$), `DcBlocker(10 Hz)` y de-énfasis complementario. Ambas ramas se mezclan mediante `ModeCrossfader` ($6\text{ ms}$). *(Nota: tras `Soft Clip`, la decimación HIIR puede añadir un sobreimpulso de Gibbs moderado sobre señales muy saturadas; cuando se requiera un techo digital estricto de $0.00\text{ dB}$, colocar `BrickwallLimiter` al final de la cadena).*

### Criterios verificados automáticamente en `crates/dsp/src/lib.rs`
1. **Noise Gate:** eliminación de *chatter* (`1` transición con histéresis de $6\text{ dB}$ frente a $\ge 4$ sin ella), rechazo total de hum a $60\text{ Hz}$ (`< 1e-5`), precisión de `Range = -20 dB` ($\pm 0.5\text{ dB}$) y conservación de transitorio inicial con *lookahead* de $1\text{ ms}$.
2. **Compresor Óptico:** recuperación dual-stage ($35\text{–}75\%$ en $150\text{ ms}$), cola dependiente de duración ($> +10\%$ residual a $+1\text{ s}$ tras ráfaga de $4\text{ s}$ frente a $0.3\text{ s}$), `THD < 1.5%` en Mi grave ($82.41\text{ Hz}$) y exactitud de `Mix = 0%`.
3. **Limitador Brickwall:** captura de picos inter-muestra (`fs/4` a $+2.9\text{ dBTP}$, `sample_peak <= 0.75·C` y `true_peak_out <= 1.02·C`), **$0.00\text{ dB}$ *overshoot*** e invarianza de latencia y recuperación en `44.1`, `48`, `96` y `192 kHz`.
4. **Soft Clip / Tape:** `THD < -80 dB` y error `< 0.05 dB` a `-30 dBFS`, reducción de aliasing `> 20 dB` en `4,370 Hz` frente a recorte duro `1×` (verificado tanto a `44.1 kHz` como a `48 kHz`), respuesta plana en `Tape` a `-40 dBFS`, compresión de agudos a `6 kHz` (`> 1.5 dB` respecto a `200 Hz`) y presencia de $H_2 > -50\text{ dB}$.

### Decisión técnica sobre `SoftClipTape` y medición coherente (`N = 4096`)
Se mantuvo intacta la arquitectura DSP de `SoftClipTape` (`ParametricSoftClipAdaa1` + `Oversampler` HIIR `4×` para `44.1/48 kHz`), descartando cambios innecesarios al *waveshaper* o al orden de sobremuestreo. Medido sobre una ventana coherente real de $N = 4096$ muestras a `4,370 Hz` (`Drive +18 dB`, `Ceiling -1 dB`, `Knee 50%`), el procesador logra **`81.63 dB` de ASNR** frente a `19.78 dB` del recorte duro `1×` (**`+61.85 dB`** de supresión de aliasing a `48 kHz`, y **`78.70 dB` vs. `19.69 dB` = `+59.01 dB`** a `44.1 kHz`).

La falla inicial observada en laboratorio (`+18.98 dB < 20 dB`) se debió exclusivamente a dos errores de medición que quedaron corregidos:
1. **Ventana no coherente en el test (`crates/dsp/src/lib.rs`):** generar `generate_coherent_sine(..., WARMUP + N, ...)` (`6144` muestras) y analizar `&[WARMUP..]` (`N = 4096`) dejaba `372.667` ciclos en la ventana FFT, introduciendo fuga espectral. Se corrigió generando el período base coherente sobre `N = 4096` y repitiéndolo cíclicamente hasta `WARMUP + N` (y validando `44.1 kHz` y `48 kHz`).
2. **Cuantización de fundamental en `compute_harmonic_metrics` (`crates/dsp/src/analyzer.rs`):** `fund_hz` se calculaba como `fund_bin as f32 * bin_hz` ignorando `known_fundamental_hz = Some(f)`, lo que acumulaba error de bin $\times m$ en el rastreo y enmascarado de armónicos altos (`m = 5`). Se corrigió para emplear directamente `known_fundamental_hz` cuando es finito y positivo.
