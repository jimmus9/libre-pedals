# Capítulo 4 — Filtros, Gabinete (IR), Modulación, Delay y Reverb

> **Estado:** ✅ Cerrado (`v1.1` — `D13` Opción B; `D14`–`D17` Opción A).  
> **Pedales cubiertos:** EQ Paramétrico *(4 bandas)* · Wah / Auto-Wah · Simulador de Gabinete (*Cabinet IR* por convolución) · Chorus, Phaser y Tremolo · Delay (*Digital / Analógico BBD / Tape*) · Reverb (*Plate / Hall / Spring*).  
> **Base previa aplicada:** fórmulas *matched* de Vicanek/Orfanidis sin *cramping* para EQ en todas las $f_s$ (`D2`), filtros modulables Cytomic SVF-TPT (`D2`), selectores `Stepped` con *crossfade* ($5\text{–}10\text{ ms}$), acumulación `f64` (`D3`) y protección FTZ/DAZ contra *denormals*.

---

## 1. Ecualización: EQ Paramétrico y EQ Gráfico

A diferencia del control de tono simple dentro de un overdrive, la ecualización independiente antes y después de la distorsión moldea el carácter fundamental de la guitarra: un EQ **pre-distorsión** decide qué frecuencias excitan el clipping (por ejemplo recortando graves embarrados o empujando medios a $800\text{ Hz}$), mientras un EQ **post-distorsión** esculpe la presencia y encaje en la mezcla.

### 1.1 EQ Paramétrico (4 bandas + Nivel)
Siguiendo la decisión `D2` del Capítulo 1, todas las bandas emplean **filtros *matched* de Vicanek (2016) y Orfanidis (1997)** en todo el rango de $f_s$ ($44.1\text{–}192\text{ kHz}$), preservando la forma analógica sin *Nyquist cramping* en agudos ni oversampling adicional:
- **Banda 1 (`Low Shelf`):** frecuencia de corte $40\text{–}400\text{ Hz}$, ganancia $\pm 15\text{ dB}$.
- **Bandas 2 y 3 (`Bell / Peaking`):** frecuencia $100\text{ Hz}\text{–}8\text{ kHz}$, ganancia $\pm 15\text{ dB}$, factor de calidad $Q \in [0.4, 8.0]$.
- **Banda 4 (`High Shelf`):** frecuencia de corte $1.5\text{–}12\text{ kHz}$, ganancia $\pm 15\text{ dB}$.
- **Control `Level`:** compensación de salida $\pm 12\text{ dB}$ suavizada por muestra.

### 1.2 EQ Gráfico de guitarra (7 bandas estilo Boss GE-7)
Un clásico pedal gráfico de guitarra (7 bandas de octava: **$100, 200, 400, 800, 1600, 3200, 6400\text{ Hz}$**, rango $\pm 15\text{ dB}$ más `Level`) plantea un problema matemático documentado por **Välimäki & Liski (IEEE SPL, 2017; DAFx-17)**:
- **Problema de interacción entre bandas vecinas:** debido al solapamiento de las curvas de campana (*band interaction*, especialmente en topologías de $Q$ constante o proporcional), si el usuario sube simultáneamente dos o tres bandas adyacentes en una cascada ingenua de filtros biquad, sus colas se suman y la respuesta real **excede con creces la curva dibujada por los deslizadores** (*overshoot* o *sag*).
- **Solución de Välimäki & Liski (*Accurate Cascade Graphic Equalizer*, 2017):** utilizando únicamente **una sección de 2.º orden por banda**, calculan los pesos reales de cada filtro aplicando una **matriz de interacción precalculada** (ajustada mediante mínimos cuadrados considerando principalmente las dos bandas adyacentes de cada filtro):
  $$\mathbf{g}_{\text{cmd}} = \mathbf{B} \mathbf{g}_{\text{filter}} \quad \Longrightarrow \quad \mathbf{g}_{\text{filter}} = \mathbf{B}^{-1} \mathbf{g}_{\text{target}}$$
  Como la matriz $\mathbf{B}^{-1}$ ($7 \times 7$) depende solo de las frecuencias fijas y del $Q$ elegido, se calcula al inicializar (`prepare()`), logrando en tiempo real una **precisión mejor que $\pm 1\text{ dB}$** respecto a la curva gráfica objetivo con mínimo costo de CPU.

> **Decisión `D13` (Opción B):** en la implementación de la galería se incluye únicamente el **EQ Paramétrico** (aprovechando las múltiples instancias pre y post distorsión). La formulación matricial de **Välimäki & Liski (2017)** queda documentada aquí por rigor científico como referencia si se decidiera añadir un EQ gráfico en versiones futuras.

---

## 2. Wah / Auto-Wah (*Envelope Filter*)

En el estudio físico del **Dunlop CryBaby GCB-95** realizado por **Holters & Zölzer (DAFx)** mediante análisis nodal DK, el carácter vocal del Wah clásico no viene de un pasa-banda con $Q$ fija:
- Al girar el potenciómetro del pedal desde el talón hacia la punta, la frecuencia central del pico resonante barre aproximadamente de **$\sim 350\text{ Hz}$ a $\sim 2.2\text{ kHz}$**, pero al mismo tiempo **el factor $Q$ y la ganancia pico se estrechan/elevan dinámicamente** al subir hacia la zona vocal media-alta debido a la carga de la red RC junto al inductor Fasel.
- **Implementación estable en tiempo real:** porque el corte y la resonancia se mueven en cada muestra (ya sea por el pie o por un seguidor de envolvente rápido), **Cytomic SVF-TPT** (`D2`) es indispensable para evitar explosiones numéricas o *zipper noise*, modulando $(f_c, k = 1/Q)$ acoplados sobre una curva calibrada al barrido del CryBaby.
- **Modos (`Stepped` con *crossfade* de $5\text{ ms}$):**
  1. **`Auto-Wah` (Envelope Filter):** un seguidor de envolvente dedicado (con `Sensitivity`, `Attack/Release` musicales y dirección **`Up / Down`**) mueve el barrido según la fuerza de la púa.
  2. **`Manual` (Pedal):** perilla continua **`Position` ($0\dots 100\%$)** lista para automatizarse o mapearse a un pedal de expresión MIDI en fases posteriores, más perilla **`Resonance`** y mezcla opcional **`Mix`**.

---

## 3. Simulador de Gabinete — *Cabinet IR* de Latencia Cero

Un amplificador o overdrive de guitarra eléctrica sin altavoz suena artificialmente áspero y estridente por encima de $4\text{–}5\text{ kHz}$. El conjunto altavoz-caja-micrófono actúa como un filtro acústico complejo de **respuesta al impulso (IR)** de $20\text{ a }100\text{ ms}$ (aprox. $1000\text{ a }19\,200$ muestras según $f_s$): resonancia mecánica grave en $\sim 80\text{–}120\text{ Hz}$, múltiples picos y cancelaciones en medios propios del cono, y caída natural pronunciada por encima de $5\text{–}6\text{ kHz}$.

### Convolución particionada híbrida sin retardo Entrada-Salida (Gardner, JAES 1995)
La convolución directa en el dominio del tiempo exige $\mathcal{O}(L)$ operaciones por muestra (inviable a $192\text{ kHz}$ con IRs largas), mientras que una convolución por bloques FFT tradicional introduce una **latencia fija igual al tamaño de bloque $B$**. La solución estándar de latencia cero, publicada por **William G. Gardner (JAES, 1995)** y formalizada en variantes uniformes (**UPOLS**, *Uniform Partitioned Overlap-Save*):

1. **Bloque de cabeza ($h_0$, tamaño $B_0 = 64$ muestras a $48\text{ kHz}$ / $256$ a $192\text{ kHz}$):** se convoluciona **en tiempo directo** (o con sub-particiones inmediatas), entregando **$0$ muestras de latencia añadida** en la salida.
2. **Cola restante ($h_1, h_2, \dots, h_{P-1}$):** se divide en particiones uniformes de tamaño $B$, se precalcula su FFT una sola vez al cargar el IR en un hilo auxiliar, y en el hilo de audio solo se ejecutan multiplicaciones complejas y suma en un *FDL* (*Frequency-Domain Delay Line*), aplicando una FFT inversa por bloque sin ninguna reserva dinámica de memoria.

### Características funcionales para `libre-pedals`
- **3 IRs libres integradas en el binario** (diseñadas/curadas sintética o libremente con licencia compatible GPLv3): `1x12 Clean`, `2x12 Crunch` y `4x12 High-Gain`.
- **Carga de archivos `.wav` externos** del usuario (con resampleo *offline* automático al cargar desde la tasa del `.wav` hacia la tasa de procesado activa y normalización de energía RMS para evitar saltos bruscos de volumen).
- Filtros `Low-Cut` ($20\text{–}250\text{ Hz}$) y `High-Cut` ($3\text{–}18\text{ kHz}$) con SVF-TPT para limpiar graves subsónicos o agudos del micro.

---

## 4. Modulación: Chorus, Phaser y Tremolo

### 4.1 Retardo fraccional bajo modulación rápida (Laakso et al., IEEE SPM 1996)
En efectos modulados por un LFO (Chorus, Tape Delay *wow/flutter*), el retardo $D[n] = \lfloor D \rfloor + \delta$ varía continuamente en cada muestra. Siguiendo la taxonomía de **Laakso, Välimäki, Karjalainen & Laine (1996)**:
- Los filtros *all-pass* de **Thiran** tienen respuesta plana en magnitud para retardos estáticos, pero al modular $\delta[n]$ rápido cambian sus polos recursivos y pueden introducir transitorios en el estado interno.
- La **interpolación polinómica cúbica de Hermite (4 puntos)** es FIR sin realimentación, mantiene pendiente $C^1$ continua en el cruce de muestras enteras, evita transitorios internos al modular rápido y entrega excelente transparencia hasta alta frecuencia con costo muy bajo.

### 4.2 Topologías por pedal
1. **Chorus *(estilo Boss CE-2 / BBD — Raffel & Smith, DAFx 2010)*:**
   - Línea de retardo corta modulada por LFO senoidal/triangular ($\sim 5\text{–}15\text{ ms}$, profundidad controlada por `Depth` y velocidad por `Rate`) usando interpolación **Hermite cúbica**.
   - **Coloración BBD opcional / sutil:** siguiendo a **Raffel & Smith (2010)**, incluye un filtro pasa-bajos suave anti-imagen (~$7\text{–}10\text{ kHz}$) en la rama húmeda para dar la calidez redonda de un BBD analógico al mezclar (`Mix`) con la señal seca.
2. **Phaser *(estilo MXR Phase 90 — Kiiski, Esqueda & Välimäki, DAFx 2016)*:**
   - Siguiendo el modelo *Gray-Box* de **Kiiski et al. (DAFx-16)**: cascada de **4 o 6 etapas *all-pass* de 1.er orden** implementadas en topología TPT (estables bajo modulación por LFO), cuyas frecuencias de quiebre se barren exponencialmente entre $\sim 200\text{ Hz}$ y $\sim 3.5\text{ kHz}$.
   - Al sumar la salida invertida/directa con la señal original aparecen $2$ o $3$ muescas (*notches*) móviles en el espectro. El parámetro `Feedback` realimenta la salida hacia la entrada para acentuar el carácter resonante.
3. **Tremolo *(modos `Stepped`: Óptico / Harmonic)*:**
   - **`Óptico`:** modulación de amplitud suave tipo fotoresistencia con LFO moldeable (senoidal a cuadrada suavizada) sin clics.
   - **`Harmonic Tremolo` *(estilo amplis Fender Brownface)*:** divide la señal mediante un *crossover* SVF-TPT complementario (~$600\text{–}800\text{ Hz}$) en banda grave y aguda, y modula ambas en **contrafase ($180^\circ$)**; al sumarlas produce una sensación espacial cálida y casi rotatoria muy musical en acordes limpios.

---

## 5. Efectos de Tiempo y Espacio: Delay y Reverb

### 5.1 Delay con 3 caracteres clásicos (`Stepped`: Digital / Analógico BBD / Tape)
Compartiendo una única línea de retardo prealojada (hasta $2.0\text{ s}$ a $192\text{ kHz}$, interpolación Hermite cúbica y control `Time` suavizado exponencialmente para lograr el deslizamiento suave de tono tipo analógico sin clics al girar la perilla):

| Carácter | Lazo de realimentación (`Feedback`) | Efecto audible tras varias repeticiones |
|---|---|---|
| **Digital** | Lazo lineal de banda completa (solo limitador suave interno de seguridad si `Feedback > 95%`) | Repeticiones cristalinas y fieles a la señal original |
| **Analógico BBD *(Raffel & Smith 2010)*** | Filtro anti-aliasing/reconstrucción multi-polo (~$2.5\text{–}4.5\text{ kHz}$) + compansión y saturación suave en cada vuelta del lazo | Cada eco se oscurece, engorda y retrocede naturalmente sin tapar la guitarra principal |
| **Tape Delay *(Chowdhury 2019)*** | Saturación suave tipo cinta + pasa-altos (~$90\text{ Hz}$) y pasa-bajos (~$4\text{ kHz}$) progresivos + modulación lenta *wow & flutter* en la lectura | Ecos orgánicos con ligera deriva de tono, aire cálido y decaimiento aterciopelado |

Controles principales: `Time` ($20\text{–}1500\text{ ms}$), `Feedback` ($0\dots 95\%$), `Tone / Mod`, `Mix` ($0\dots 100\%$) y selector `Mode` (`Digital / BBD / Tape`).

### 5.2 Reverb con 3 topologías (`Stepped`: Plate / Hall / Spring)

1. **Plate — Topología de Jon Dattorro (JAES, 1997):**
   - Del paper *"Effect Design, Part 1: Reverberator and Other Filters"*: filtrado de ancho de banda inicial + **4 difusores *all-pass* de entrada** que convierten el ataque de púa en una nube densa sin ecos discretos metálicos, alimentando un **tanque en figura-ocho (*figure-8*)** cruzado con líneas de retardo moduladas por LFO lento y amortiguamiento (*damping*) de agudos por *one-pole*.
2. **Hall — FDN de 8 canales con matriz ortogonal (*Hadamard / Householder*):**
   - Ocho líneas de retardo mutuamente primas (escaladas con $f_s$) mezcladas en cada paso por una **matriz unitaria de Hadamard** ($H_8 / \sqrt{8}$) usando solo sumas, restas y un escalado fijo, garantizando conservación de energía, alta densidad modal y decaimiento natural con **`Pre-Delay`** configurable.
3. **Spring Reverb *(Välimäki, Parker, Abel & Smith, JAES 2010)*:**
   - Recrea el tanque de resortes clásico de amplificador mediante una **cadena dispersiva de filtros *all-pass* de 1.er orden en cascada** acoplada en bucle a un retardo con *damping*: por debajo de la frecuencia de transición ($f_t \approx 3.5\text{–}4.5\text{ kHz}$), las distintas frecuencias viajan a distinta velocidad, produciendo el inconfundible *"chirp"* elástico metálico ante ataques fuertes de púa.

Controles principales: `Decay` (`RT60`), `Pre-Delay` ($0\dots 100\text{ ms}$), `Tone / Damping`, `Mix` ($0\dots 100\%$) y selector `Mode` (`Plate / Hall / Spring`).

---

## 6. Coherencia con Capítulos 1–3 (memoria estática a $192\text{ kHz}$, mono/estéreo y pruebas)

1. **Preasignación estricta en `prepare(sample_rate)` hasta $192\text{ kHz}$:**
   - A $192\text{ kHz}$, $2.0\text{ s}$ de retardo equivalen a $384\,000$ muestras ($\approx 1.5\text{ MB}$ por instancia de Delay) y una IR de $100\text{ ms}$ requiere $19\,200$ muestras. Toda reserva de buffers (`Vec`, particiones FFT y líneas de retardo) se ejecuta **únicamente en `prepare()`**, nunca en `process()`, asegurando cero bloqueos en tiempo real.
   - Si en el motor existen hasta 4 instancias por tipo de pedal, las instancias no montadas solo reservan su buffer completo cuando `prepare` las inicializa o con capacidad acotada compartida/bajo demanda fuera del hilo de audio si hiciera falta optimizar RAM (nota a cuidar al implementar `Delay` y `Cabinet IR`).
2. **Compatibilidad mono hoy y salida estéreo mañana:**
   - El trait `Effect::process(&mut [f32])` actual del repositorio es **mono**. Todas las arquitecturas descritas (incluida Dattorro Plate y FDN Hall sumadas a mono limpiamente sin cancelaciones de fase) funcionan en mono en la v1 y están diseñadas con ramas simétricas para exponerse fácilmente en estéreo si se amplía el bus del motor en el futuro.
3. **Pruebas matemáticas de verificación por bloque:**
   - **EQ Gráfico:** barrido de respuesta en frecuencia verificando error $< \pm 1\text{ dB}$ respecto a las ganancias de las 7 bandas; cero *cramping* en EQ Paramétrico en $44.1\text{–}192\text{ kHz}$.
   - **Cabinet IR (UPOLS):** identidad matemática contra convolución directa en el tiempo ($\text{ESR}_{\text{dB}} < -120\text{ dB}$) y verificación de **latencia $= 0$ muestras** en impulso unitario.
   - **Modulación / Delay:** ausencia de artefactos discontinuos al modular retardo Hermite y verificación de caída espectral progresiva en lazo BBD/Tape.
   - **Reverb:** estabilidad de energía (ausencia de auto-oscilación o desbordamiento cuando `Decay < 100%`), protección FTZ/DAZ en colas largas y perfil dispersivo del modo `Spring`.

---

## 7. Bibliografía del Capítulo 4

1. **Orfanidis, S. J. (1997).** *"Digital Parametric Equalizer Design with Prescribed Nyquist-Frequency Gain"*. *JAES* (EQ sin *cramping*).
2. **Vicanek, M. (2016).** *"Matched Second Order Digital Filters"* y *"Matched Shelving Filters"*.
3. **Välimäki, V., & Liski, J. (2017).** *"Accurate Cascade Graphic Equalizer"*. *IEEE Signal Processing Letters*, vol. 24, n.º 2, pp. 176–180; y **Liski, J., & Välimäki, V. (2017)**, *"The Quest for the Best Graphic Equalizer"*, DAFx-17.
4. **Holters, M., & Zölzer, U.** *"Physical Modelling of a Wah-Wah Effect Pedal as a Case Study for Application of the Nodal DK Method to Circuits with Variable Parts"*, DAFx (Dunlop CryBaby GCB-95).
5. **Gardner, W. G. (1995).** *"Efficient Convolution without Input/Output Delay"*. *Journal of the Audio Engineering Society (JAES)*, vol. 43, n.º 3, pp. 127–136 (base de convolución particionada de latencia cero / UPOLS).
6. **Laakso, T. I., Välimäki, V., Karjalainen, M., & Laine, U. K. (1996).** *"Splitting the Unit Delay: Tools for Fractional Delay Filter Design"*. *IEEE Signal Processing Magazine*, vol. 13, n.º 1, pp. 30–60.
7. **Raffel, C., & Smith, J. O. (2010).** *"Practical Modeling of Bucket-Brigade Device Circuits"*. *DAFx-10*, Graz (BBD anti-aliasing, reconstrucción y compansión).
8. **Kiiski, R., Esqueda, F., & Välimäki, V. (2016).** *"Time-Variant Gray-Box Modeling of a Phaser Pedal"*. *DAFx-16*, Brno (MXR Phase 90).
9. **Dattorro, J. (1997).** *"Effect Design, Part 1: Reverberator and Other Filters"*. *Journal of the Audio Engineering Society (JAES)*, vol. 45, n.º 9 (reverb Plate figura-ocho).
10. **Välimäki, V., Parker, J. D., & Abel, J. S. (2010).** *"Parametric Spring Reverberation Effect"*. *JAES*, vol. 58, n.º 7/8, pp. 547–562 (y Abel et al., AES 2006).

---

## 8. Decisiones cerradas (`v1.1`)

Con estas últimas 5 decisiones (`D13`–`D17`) se cierra la fase de investigación completa de los Capítulos 1–4 antes de iniciar la implementación por olas en código:

| ID | Pedal / Familia | Opciones consideradas | Recomendación inicial | Decisión final |
|---|---|---|---|---|
| **D13** | **Ecualizadores (`EQ`)** | **A)** Ambos pedales separados: **EQ Paramétrico** (4 bandas *matched* Vicanek/Orfanidis) + **EQ Gráfico 7 bandas** (estilo GE-7 con matriz de Liski-Välimäki)<br>**B)** Solo EQ Paramétrico | Opción A | ✅ **Opción B**: solo **EQ Paramétrico** (4 bandas *matched* + `Level`), usando múltiples instancias si se requiere pre/post distorsión |
| **D14** | **`Wah / Auto-Wah`** | **A)** Un solo pedal **`Wah`** con selector `Stepped` **`Auto-Wah` / `Manual`** sobre SVF-TPT calibrado al CryBaby (`Sensitivity`/`Position`, `Resonance`, `Mix`, dirección)<br>**B)** Dos pedales separados | Opción A | ✅ **Opción A**: único pedal **`Wah`** con selector `Stepped` (`Auto-Wah / Manual`) sobre SVF-TPT |
| **D15** | **`Cabinet IR` (Convolución)** | **A)** Convolución híbrida de latencia cero (cabeza directa + cola FFT UPOLS) con **3 IRs integradas** + carga de `.wav` externos + `Low-Cut`/`High-Cut`<br>**B)** Solo las 3 IRs integradas sin carga de `.wav` | Opción A | ✅ **Opción A**: convolución UPOLS de latencia cero con **3 IRs libres integradas** + carga de `.wav` externos + `Low-Cut`/`High-Cut` |
| **D16** | **`Modulación`** | **A)** Tres pedales independientes y simples: **`Chorus`**, **`Phaser`** y **`Tremolo`** (`Óptico / Harmonic`)<br>**B)** Un único multi-pedal de modulación | Opción A | ✅ **Opción A**: tres pedales independientes (**`Chorus`**, **`Phaser`**, **`Tremolo`**) para poder intercalarlos libremente en la cadena |
| **D17** | **`Delay` y `Reverb`** | **A)** Un pedal **`Delay`** multimodo (`Digital / BBD / Tape`) y un pedal **`Reverb`** multimodo (`Plate / Hall / Spring`)<br>**B)** Pedales separados para cada carácter (6 pedales) | Opción A | ✅ **Opción A**: **`Delay`** multimodo (`Digital / BBD / Tape`) y **`Reverb`** multimodo (`Plate / Hall / Spring`) manteniendo 13 familias ordenadas |
