# Capítulo 1 — Fundamentos DSP transversales

> **Estado:** ✅ Cerrado (`v1.2` — revisado con decisiones `D1`–`D4` aprobadas, soporte de selectores discretos `Stepped` y métrica `ESR`; revisable si capítulos posteriores lo requieren).  
> **Enfoque:** Fundamentos independientes del lenguaje con criterios prácticos de implementación en tiempo real (`f32`/`f64`, cero reservas de memoria en el hilo de audio, baja latencia).

---

## Resumen ejecutivo y alcance

Antes de diseñar cada pedal de la galería (overdrives, preamp de válvula, compresor óptico, limitadores, EQ, wah, modulación, delay y reverb), toda la cadena comparte **cuatro cimientos matemáticos transversales**:

1. **Antialiasing en procesos no lineales:** evitar que los armónicos generados por encima de Nyquist ($f_s/2$) se plieguen (*aliasing* / *foldback*) creando disonancias inarmónicas y "aspereza digital" en overdrives, limitadores, tape saturators y preamps 12AX7.
2. **Filtros estables bajo modulación y sin deformación en agudos:** garantizar que los filtros puedan cambiar de frecuencia o resonancia por muestra sin explotar ni sonar artificiales, y que el EQ no se deforme cerca de $f_s/2$ (*Nyquist cramping*).
3. **Control sin clics y protección contra *denormals*:** suavizar cambios de perillas y presets sin *zipper noise*, y evitar caídas bruscas de rendimiento por números subnormales en colas de IIR, delays y reverbs.
4. **Métricas objetivas de calidad y naturalidad armónica:** un protocolo de medición reproducible (THD, relación armónicos pares vs. impares, IMD y piso de aliasing) para validar cada pedal matemáticamente, además de escucharlo.

**Contexto del motor actual de `libre-pedals`:** el motor procesa a la frecuencia nativa de entrada (habitualmente $44.1\text{ kHz}$ o $48\text{ kHz}$ en interfaces/micrófonos estándar, y hasta $192\text{ kHz}$ en tarjetas configuradas en alta resolución), aplicando cualquier resampleo necesario únicamente al final hacia la salida. Por ello, todas las técnicas de este capítulo deben escalar de forma coherente desde $44.1\text{ kHz}$ hasta $192\text{ kHz}$ sin desperdiciar CPU ni añadir latencia innecesaria.

---

## 1. Antialiasing en procesos no lineales

### 1.1 El problema del aliasing en saturación
Una función no lineal sin memoria $y = f(x)$ aplicada sobre una señal de frecuencia $f_0$ genera nuevos armónicos en múltiplos enteros $k f_0$. En tiempo continuo esto enriquece el timbre; en tiempo discreto a frecuencia de muestreo $f_s$, todo armónico que supere el límite de Nyquist ($f_s / 2$) se refleja hacia frecuencias inferiores dentro de la banda audible:

$$f_{\text{alias}}(k) = \left| k f_0 - m f_s \right| \le \frac{f_s}{2}, \quad m \in \mathbb{Z}$$

A diferencia de los armónicos naturales ($2f_0, 3f_0, \dots$), estos reflejos casi nunca son múltiplos musicales de $f_0$. En guitarra eléctrica —donde acordes con quintas o notas agudas con *overdrive* o *clipping* producen armónicos hasta $10\times\text{–}30\times$ la fundamental— el aliasing se percibe como un zumbido metálico o suciedad inarmónica, especialmente evidente al hacer *bends*.

Existen tres grandes familias de solución en la literatura DSP moderna: **Oversampling clásico**, **ADAA** (*Antiderivative Antialiasing*) y los **esquemas híbridos**.

---

### 1.2 Enfoque A — Oversampling clásico (Interpolación → Procesado → Decimación)

Consiste en elevar la frecuencia de muestreo por un factor $M$ mediante un filtro pasa-bajos interpolador (eliminando imágenes espectrales), ejecutar la no linealidad a $M \cdot f_s$, y filtrar por debajo de $f_s/2$ antes de diezmar de vuelta a $f_s$. La elección de los filtros de subida y bajada define la latencia, el costo de CPU y la transparencia.

#### 1) Filtros Half-Band IIR polifásicos elípticos (HIIR — Laurent de Soras)
- **Topología:** descompone un filtro elíptico *half-band* (cuya banda de paso y de parada son simétricas alrededor de $f_s/4$) en **dos ramas *all-pass* en paralelo** formadas por secciones de 1.er y 2.º orden en cascada, con un retardo de una muestra en una de las ramas.
- **Cascada $2\times \to 4\times \to 8\times$:** cada etapa duplica o divide por dos la tasa de muestreo. Las etapas superiores requieren menos coeficientes porque la banda de transición relativa es mucho más ancha a $4f_s$ u $8f_s$ que a $2f_s$.
- **Ventajas para guitarra en vivo:**
  - **Fase mínima:** cero *pre-ringing* antes de los ataques de púa y latencia grupal muy baja (del orden de unas pocas muestras).
  - **Costo computacional muy reducido:** logra atenuaciones de banda de parada de $-80\text{ a }-100\text{ dB}$ con muy pocas operaciones por muestra.
- **Contrapartida:** ligera dispersión de fase cerca de Nyquist (prácticamente inaudible en audio de guitarra, aunque relevante si se mezcla la señal procesada con la limpia en paralelo sin alinear fase).

#### 2) Filtros FIR Half-Band (fase lineal vs. fase mínima)
- **Fase lineal (FIR simétrico):** mantiene la forma de onda temporal exactamente alineada en todas las frecuencias (útil si hubiera mezcla *dry/wet* estricta en fase o medición temporal), aprovechando además que casi la mitad de los coeficientes de un FIR *half-band* son cero.
- **Contrapartida:** un filtro FIR con caída pronunciada y alta atenuación exige un orden elevado $N$, lo que añade una **latencia fija de $N/2$ muestras** por etapa y genera **pre-ringing** (oscilaciones previas al transitorio) en ataques rápidos de púa. Además consume más CPU que una red HIIR equivalente.

#### 3) Hallazgo de Kahles, Esqueda & Välimäki (JAES 2019)
En *"Oversampling for Nonlinear Waveshaping: Choosing the Right Filters"* (JAES, vol. 67, n.º 6, pp. 440–449, 2019), los autores evaluaron distintas combinaciones de filtros de interpolación y decimación para un sistema con oversampling $8\times$ sometido al peor caso práctico (senoidales con *hard clipping*), midiendo el error mediante la relación psicoacústica ruido-a-máscara ponderada A (*A-weighted noise-to-mask ratio*):
- **Interpolación (*upsampling*):** concluyeron que basta una **interpolación lineal compensada con un filtro *high-shelf*** ligero para igualar la caída en agudos, con un costo mínimo.
- **Decimación (*downsampling*):** evaluaron filtros FIR interpolados, elípticos y CIC. El único esquema que logró supresión de aliasing psicoacústicamente suficiente en todo el espectro bajo esa configuración fue un **filtro CIC con fugas (*leaky CIC* — *Cascaded Integrator-Comb*) seguido de un *high-shelf* de 1.er orden**.
- **Lectura práctica:** aunque el esquema *leaky CIC + high-shelf* es muy atractivo para factores altos fijos como $8\times$, las cascadas **HIIR polifásicas IIR** siguen siendo la arquitectura estándar más flexible cuando se necesitan factores modulares ($2\times$, $4\times$, $8\times$) con mínima latencia y respuesta plana hasta cerca de Nyquist.

---

### 1.3 Enfoque B — ADAA (*Antiderivative Antialiasing*)

Formalizado para audio por **Parker, Zavalishin & Le Bivic (DAFx 2016)** y extendido a orden superior por **Bilbao, Esqueda, Parker & Välimäki (IEEE Signal Processing Letters, 2017)**, ADAA reduce el aliasing **en la propia tasa de muestreo** modelando una reconstrucción continua lineal (o de orden superior) entre muestras consecutivas y aplicando la no linealidad seguida de un filtro rectangular o triangular continuo resuelto analíticamente.

#### 1) ADAA de 1.er orden (`ADAA1`, Parker et al., DAFx 2016)
Sea $F_1(x) = \int f(x)\,dx$ la primera antiderivada de la función no lineal $f(x)$. La salida antialiasizada entre muestras consecutivas $x_n = x[n]$ y $x_{n-1} = x[n-1]$ es la diferencia dividida:

$$y[n] = \frac{F_1(x_n) - F_1(x_{n-1})}{x_n - x_{n-1}}$$

- **Latencia:** exactamente **$0.5$ muestras** (un retardo fraccional mínimo).
- **Limitación espectral:** el filtrado implícito equivale a un filtro de media móvil en tiempo continuo cuya respuesta en frecuencia sigue una curva $\text{sinc}$, lo que produce cierta **atenuación en agudos cerca de Nyquist** si se usa a $1\times$ a $44.1/48\text{ kHz}$ (a $96/192\text{ kHz}$ esa caída queda fuera de la banda audible).

#### 2) ADAA de 2.º orden (`ADAA2`, Bilbao et al., IEEE SPL 2017)
Introduce la segunda antiderivada $F_2(x) = \int F_1(x)\,dx$ mediante un núcleo triangular (convolución continua de orden superior), logrando una supresión de aliasing mucho más profunda:

$$\Delta f_2(x_n, x_{n-1}) = 2 \cdot \frac{(x_n - x_{n-1})\left[F_1(x_n) + F_1(x_{n-1})\right] - 2\left[F_2(x_n) - F_2(x_{n-1})\right]}{(x_n - x_{n-1})^3}$$

*(En la formulación simétrica completa se promedia sobre tres muestras $x_n, x_{n-1}, x_{n-2}$ combinando el término $\Delta f_2$ entre intervalos adyacentes, con una latencia total de ~1 muestra.)*

#### 3) Retos numéricos críticos de ADAA (Chowdhury / DAFx)
La teoría de ADAA es elegante, pero su implementación directa en punto flotante presenta **tres trampas documentadas** que condicionan cualquier diseño serio:

1. **Mal condicionamiento cuando $x_n \approx x_{n-1}$:**
   Cuando la señal cambia muy poco (silencio, frecuencias graves o mesetas planas), $|x_n - x_{n-1}| \to 0$ y tanto el numerador como el denominador tienden a cero, provocando **cancelación catastrófica** (ruido numérico o `NaN`).
   - *Solución obligatoria:* definir un umbral $\epsilon$ para $\Delta x = |x_n - x_{n-1}|$ y conmutar a la expansión de Taylor alrededor del punto medio cuando $\Delta x < \epsilon$:
     - En `ADAA1`: $y[n] \approx f\!\left(\frac{x_n + x_{n-1}}{2}\right)$.
     - En `ADAA2`: expansión analítica de derivadas/valor en el punto medio para evitar la división por $(x_n - x_{n-1})^3$.

2. **Sensibilidad extrema entre `f32` y `f64`:**
   `f32` dispone solo de ~7 dígitos decimales de precisión (épsilon de máquina $\approx 1.19 \times 10^{-7}$), frente a ~15–17 dígitos en `f64` ($\approx 2.22 \times 10^{-16}$).
   - En `ADAA2`, el denominador cúbico $(x_n - x_{n-1})^3$ amplifica los errores de redondeo mucho antes que en `ADAA1`, obligando en `f32` a usar umbrales $\epsilon$ conservadores ($\sim 10^{-3}\text{ a }10^{-4}$) o bien **evaluar la diferencia ADAA2 en `f64`** antes de devolver `f32`.

3. **Costo y estabilidad de antiderivadas cerradas (caso $\tanh$):**
   Para el saturador clásico $f(x) = \tanh(x)$:
   - **1.ª antiderivada:** $F_1(x) = \ln(\cosh(x))$. Evaluarla literalmente desborda a $+\infty$ cuando $|x|$ supera ~88 en `f32` (y en señales con mucho *drive* es fácil alcanzar valores altos). Se resuelve con la identidad asintótica estable:
     $$F_1(x) = |x| - \ln(2) + \ln\!\left(1 + e^{-2|x|}\right)$$
     donde $e^{-2|x|} \in (0, 1]$ nunca desborda.
   - **2.ª antiderivada:** requiere el **dilogaritmo** $\text{Li}_2$:
     $$F_2(x) = \frac{1}{2}\left(\text{Li}_2\!\left(-e^{-2x}\right) - x\left(x + 2\ln\!\left(e^{-2x}+1\right) - 2\ln(\cosh(x))\right)\right)$$
     Calcular $\text{Li}_2$ sin ramificaciones ni errores de dominio en tiempo real es lento y complejo. Como alternativas prácticas documentadas existen:
     - **ADAA-LUT:** preintegrar numéricamente $F_1$ y $F_2$ al inicializar en tablas de búsqueda (*Lookup Tables*) con interpolación lineal o cúbica, válido para cualquier curva empírica o de diodo/válvula sin forma cerrada.
     - **Aproximaciones algebraicas racionales** (p. ej. $x / (1 + |x|)$ o polinomios por tramos), cuyas antiderivadas 1.ª y 2.ª solo usan aritmética básica y un `ln` simple.

#### 4) Extensión a sistemas con estado (*Stateful ADAA*)
Las ecuaciones anteriores aplican a no linealidades sin memoria. Para sistemas donde el elemento no lineal vive dentro de lazos con capacitores o inductores (por ejemplo, diodos en el lazo de realimentación de un Tube Screamer o etapas de tríodo), **Holters (DAFx 2019)** y **Albertini, Bernardini & Sarti (DAFx 2020)** extendieron el concepto de ADAA a sistemas con estado y **Wave Digital Filters (WDF)**, permitiendo aplicar antialiasing localmente en cada no linealidad del circuito. Evaluaremos su viabilidad frente al oversampling en el Capítulo 2.

---

### 1.4 Enfoque C — Híbrido (Oversampling bajo $2\times / 4\times$ + ADAA1)

Combinar una cascada ligera de oversampling (por ejemplo $2\times$ o $4\times$ con filtros polifásicos IIR) con `ADAA1` en el *waveshaper* es una de las estrategias más eficaces en DSP moderno:
- El oversampling $2\times/4\times$ desplaza la caída $\text{sinc}$ en agudos de `ADAA1` por encima de $20\text{ kHz}$, dejando la banda audible plana.
- La supresión combinada de aliasing rivaliza con un oversampling puro de $8\times$–$16\times$, pero con una fracción de CPU, menor latencia y usando solo `ADAA1` (sin dilogaritmos ni denominador cúbico inestable).

---

### 1.5 Comparativa de estrategias de antialiasing

| Estrategia | Rechazo de aliasing | Latencia típica | Fase / Pre-ringing | Costo de CPU | Complejidad / Estabilidad `f32` |
|---|---|---|---|---|---|
| **Oversampling puro $4\times/8\times$ (HIIR IIR)** | Muy alto (dependiente del orden del filtro y factor $M$) | Muy baja (pocas muestras, fase mínima) | Sin *pre-ringing*; leve dispersión cerca de Nyquist | Medio / Alto a $8\times$ | Baja; muy estable en `f32` si cada etapa está bien diseñada |
| **Oversampling FIR fase lineal ($4\times/8\times$)** | Muy alto | Media-Alta ($N/2$ muestras por etapa) | Fase lineal; posible *pre-ringing* en transitorios | Alto | Baja; estable en `f32` |
| **ADAA1 solo ($1\times$)** | Moderado-Bueno (equiv. aprox. a $2\times\text{–}3\times$) | $0.5$ muestras | Atenuación suave en agudos cerca de Nyquist ($\text{sinc}$) | Muy bajo | Baja-Media; requiere umbral $\epsilon$ y truco estable en `ln(cosh)` |
| **ADAA2 solo (`f64` o LUT)** | Alto (equiv. aprox. a $4\times\text{–}6\times$) | ~1 muestra | Menor caída en agudos que ADAA1 | Medio (alto si usa $\text{Li}_2$; bajo con LUT) | Alta; muy sensible a cancelación en `f32` (conviene `f64` o LUT) |
| **Híbrido $2\times/4\times$ HIIR + ADAA1** | **Muy alto / Excelente** | **Muy baja** | **Plana en banda audible**; sin *pre-ringing* | **Bajo-Medio** | Media; estable en `f32` |

#### Implicación clave del *sample rate* adaptativo (partición continua sin huecos)
Si la interfaz del usuario ya corre a **$192\text{ kHz}$**, la frecuencia de Nyquist es $96\text{ kHz}$ ($4\times$ la de $48\text{ kHz}$). Aplicar ciegamente un oversampling $8\times$ llevaría la saturación a $1.536\text{ MHz}$ sin beneficio audible pero multiplicando la CPU por 8. Para evitar rangos sin definir en todo el espectro $(0, \infty)$, el factor de oversampling $M(f_s)$ se define de forma **continua y total** buscando una tasa efectiva mínima de $176.4\text{ kHz}$ (múltiplo entero de $44.1\text{ kHz}$, estándar CD/audio):

$$M(f_s) = \operatorname{clamp}\!\left(2^{\max\left(0,\; \left\lceil \log_2\!\left(\frac{176\,400}{f_s}\right) \right\rceil\right)},\; 1,\; 8\right)$$

Esto cubre sin huecos cualquier frecuencia de muestreo posible:
- $f_s < 44.1\text{ kHz}$ (p. ej. $22.05\text{ kHz}$ o $32\text{ kHz}$) $\implies 8\times$ + `ADAA1`.
- $44.1\text{ kHz} \le f_s \le 48\text{ kHz}$ (estándar habitual; nota: $176\,400 / 48\,000 = 3.675 \Rightarrow \lceil \log_2 \rceil = 2$) $\implies 4\times$ ($176.4\text{–}192\text{ kHz}$ efectivos) + `ADAA1`.
- $48\text{ kHz} < f_s < 176.4\text{ kHz}$ (incluyendo $88.2\text{ kHz}$, $96\text{ kHz}$ y cualquier tasa intermedia como $64\text{ kHz}$ o $128\text{ kHz}$) $\implies 2\times$ ($>96\text{ kHz}$ a $<352.8\text{ kHz}$ efectivos) + `ADAA1`.
- $f_s \ge 176.4\text{ kHz}$ (incluyendo $176.4\text{ kHz}$ y $192\text{ kHz}$) $\implies 1\times$ + `ADAA1` *(con modo "Ultra" $2\times$ opcional más adelante)*.

---

## 2. Filtros estables, *Nyquist cramping*, suavizado y *denormals*

### 2.1 Filtros TPT / ZDF — SVF Trapezoidal (`SvfLinearTrapOptimised2`)

Los filtros biquad clásicos en **Direct Form I/II** (como los del *Audio EQ Cookbook* de Robert Bristow-Johnson) presentan dos defectos graves cuando sus parámetros cambian en tiempo real:
1. Sus estados internos almacenan combinaciones acopladas de entrada/salida pasadas; al barrer la frecuencia de corte o la resonancia $Q$ rápido (como en un **Wah / Auto-Wah**, un cambio de **Tone** o una transición entre presets), aparecen clics, saltos de ganancia transitorios o incluso inestabilidad numérica.
2. Pierden correspondencia topológica directa con el circuito analógico original.

La solución estándar moderna es la discretización mediante **TPT** (*Topology-Preserving Transform*, Vadim Zavalishin, *The Art of VA Filter Design*) con integración trapezoidal —equivalente a resolver implícitamente los lazos de realimentación sin retardo artificial (**ZDF**, *Zero-Delay Feedback*).

#### Ecuaciones exactas de Andrew Simper (Cytomic, `SvfLinearTrapOptimised2`)
Dada la frecuencia de muestreo $f_s$, frecuencia de corte $f_c$ (acotada de forma segura por debajo de $f_s/2$) y amortiguamiento $k = 1/Q$ (para $Q > 0$):

$$g = \tan\!\left(\frac{\pi f_c}{f_s}\right)$$

$$a_1 = \frac{1}{1 + g(g + k)}, \qquad a_2 = g \cdot a_1, \qquad a_3 = g \cdot a_2 = g^2 \cdot a_1$$

Para cada muestra de entrada $v_0 = x[n]$, usando los dos estados internos equivalentes de corriente `ic1eq` e `ic2eq`:

$$v_3 = v_0 - \text{ic2eq}$$

$$v_1 = a_1 \cdot \text{ic1eq} + a_2 \cdot v_3$$

$$v_2 = \text{ic2eq} + a_2 \cdot \text{ic1eq} + a_3 \cdot v_3$$

Actualización trapezoidal de estados:

$$\text{ic1eq} \leftarrow 2 v_1 - \text{ic1eq}$$

$$\text{ic2eq} \leftarrow 2 v_2 - \text{ic2eq}$$

Salidas simultáneas disponibles con la misma estructura:
- **Pasa-bajos (LP):** $y_{\text{LP}} = v_2$
- **Pasa-banda (BP):** $y_{\text{BP}} = v_1$
- **Pasa-altos (HP):** $y_{\text{HP}} = v_0 - k v_1 - v_2$
- **Notch (rechazo de banda):** $y_{\text{Notch}} = v_0 - k v_1$

*(Mediante ponderación lineal de $v_0, v_1, v_2$, la misma topología trapezoidal implementa filtros **Bell/Peaking**, **Low-Shelf**, **High-Shelf** y **All-Pass** con total estabilidad bajo modulación por LFO, envolvente o perilla.)*

**Ventaja arquitectónica:** un único módulo SVF-TPT bien probado sirve de bloque base para el tono del Overdrive, el Auto-Wah/Wah, el filtro *sidechain* del Noise Gate, la coloración de repeticiones del Delay BBD/Tape, el amortiguamiento de la Reverb y el EQ dinámico.

---

### 2.2 *Nyquist cramping* y filtros *Matched* (Orfanidis 1997; Vicanek 2016)

Aunque la integración trapezoidal (transformada bilineal, BLT) garantiza estabilidad y respuesta exacta a bajas frecuencias, comprime el eje de frecuencias infinitas del dominio analógico $[0, \infty)$ dentro de $[0, f_s/2]$ (*frequency warping*).
- **Efecto en guitarra y EQ:** a $44.1\text{ kHz}$ o $48\text{ kHz}$, una banda **Bell/Peaking** centrada en $8\text{–}14\text{ kHz}$ se vuelve artificialmente estrecha y asimétrica, y un **High-Shelf** se aplasta hacia $0\text{ dB}$ en Nyquist. A $96\text{–}192\text{ kHz}$ el problema prácticamente desaparece dentro de la banda audible ($< 20\text{ kHz}$), pero muchos usuarios operarán a $44.1/48\text{ kHz}$.

#### Soluciones documentadas en literatura y criterio unificado sin conmutación en $48\text{ kHz}$
1. **Orfanidis (JAES 1997, *"Digital Parametric Equalizer Design with Prescribed Nyquist-Frequency Gain"*):** fórmulas cerradas de biquad que fijan explícitamente la ganancia deseada en DC, frecuencia central $f_0$, ancho de banda y **ganancia en Nyquist**, preservando el lóbulo analógico sin necesitar oversampling.
2. **Martin Vicanek (2016, *"Matched Second Order Digital Filters"* y *"Matched One-Pole / Two-Pole Digital Shelving Filters"*):** derivaciones cerradas optimizadas para tiempo real (LP, HP, BP, Peaking y Shelving Butterworth) que igualan la respuesta en magnitud del prototipo analógico hasta Nyquist (e incluso más allá en magnitud) sin aplastamiento de banda.
3. **Criterio unificado (sin salto en $48\text{ kHz}$):** las fórmulas *matched* de Vicanek/Orfanidis convergen suavemente al prototipo analógico cuando $f_s$ crece hacia $96\text{–}192\text{ kHz}$, por lo que **no tiene sentido cambiar de algoritmo a $48\text{ kHz}$**:
   - **EQ Paramétrico y EQ Gráfico** usarán fórmulas *matched* en **todo** el rango de $f_s$ (sin fronteras ni saltos).
   - **Filtros modulables** (*Tone*, *Auto-Wah*, *sidechain* del Gate, coloración de *Delay BBD/Tape*) usarán **Cytomic SVF-TPT** en **todo** el rango de $f_s$, priorizando estabilidad bajo modulación rápida por muestra.

---

### 2.3 Suavizado de parámetros sin *zipper noise*

Actualizar un control (volumen, ganancia, corte de filtro) por bloque de audio (p. ej. cada 64–512 muestras) introduce escalones discretos que generan un zumbido o chasquido en alta frecuencia llamado ***zipper noise***.

| Método | Ecuación | Ventajas | Desventajas | Uso recomendado |
|---|---|---|---|---|
| **One-pole exponencial** | $y[n] = (1 - \alpha)\,\text{target} + \alpha\,y[n-1]$ con $\alpha = e^{-1/(f_s \tau)}$ | Ultra barato ($2$ mult + $1$ suma); transición suave sin esquinas en velocidad; respuesta natural tipo RC para dB/volumen y frecuencias | Asintótico (conviene hacer *snap* a `target` cuando $|y - \text{target}| < 10^{-6}$ para detener recálculo de coeficientes) | Perillas continuas de volumen, Drive, Tone, frecuencia de corte y transiciones de preset |
| **Rampa lineal** | $y[n] = y[n-1] + \frac{\text{target} - \text{start}}{N}$ | Alcanza `target` en exactamente $N$ muestras de forma determinística | Discontinuidad $C^1$ (cambio brusco de pendiente) al inicio y al final; puede generar un clic grave sutil en filtros resonantes | Fades cortos de bypass/mute o compensación de nivel fija por bloque |

**Regla clave:** suavizar en la **escala perceptiva correcta**:
- Ganancias en **lineal** (con rampa rápida de 5–15 ms) o en **dB** cuando el cambio es grande ($> 12\text{ dB}$).
- Frecuencias de filtro en **octavas / logaritmo** (o suavizando directamente `g` cuando el salto es pequeño) con $\tau \approx 5\text{–}20\text{ ms}$, evitando recalcular $\tan(\pi f_c / f_s)$ si el parámetro suavizado ya convergió (*snap*).

#### Parámetros discretos (selectores de modo) y conmutación sin clics
Muchos pedales de la galería requieren selectores discretos de 2 o 3 posiciones (por ejemplo `Op-Amp` vs. `JFET` en el *Clean Boost*, `Compress` vs. `Limit` en el *Compresor Óptico*, o `Soft Clip` vs. `Tape` en el *Limitador de Cinta*). Para mantener la arquitectura reactiva basada en metadatos sin romper presets ni la compatibilidad atómica con `AtomicF32`:
- `ParamSpec` se extenderá con una variante discreta **`Stepped`** (lista fija de etiquetas por índice), almacenada internamente como entero $0, 1, \dots$ dentro de `AtomicF32` y serializada en presets JSON por su `id`.
- **Regla anti-clics en tiempo real:** al detectar un cambio de valor en un parámetro `Stepped`, el algoritmo del pedal **no debe conmutar bruscamente** entre dos ramas de audio o dos conjuntos de constantes dispares. Debe aplicar un ***crossfade* suave de $5\text{–}10\text{ ms}$** (o interpolar suavemente sus coeficientes internos cuando ambas ramas compartan topología) para garantizar continuidad sin clics ni chasquidos.

---

### 2.4 Protección contra *denormals* (números subnormales IEEE 754)

En filtros recursivos (IIR, SVF, DC blockers), colas de Delay y redes de Reverb, cuando la señal cae al silencio la amplitud decae exponencialmente por debajo del menor número normalizado de `f32` ($\approx 1.175 \times 10^{-38}$). En arquitecturas x86/x64 estándar, operar con subnormales provoca excepciones de microcódigo que **disparan el uso de CPU entre $10\times$ y $100\times$**, ocasionando *xruns* y cortes de audio justo cuando el guitarrista deja de tocar.

- **Solución por hardware (sin sobrecarga en los lazos internos):** habilitar los bits **FTZ** (*Flush-To-Zero*) y **DAZ** (*Denormals-Are-Zero*) en el registro `MXCSR` al inicio del *callback* del hilo de audio (restaurando el estado previo al salir si el entorno lo comparte, o en hilo dedicado del motor).
- **Respaldo en código portable:** añadir sanitización de estados no finitos (`!is_finite()`) y evitar que realimentaciones queden atrapadas en estados subnormales en tests o arquitecturas sin DAX/FTZ.

---

### 2.5 Implicaciones de combinar técnicas y fronteras de cambio (análisis matemático vs. auditivo)

Al combinar técnicas (p. ej. HIIR + `ADAA1`, *fallback* de Taylor en `ADAA`, o filtros distintos según la función del pedal) conviene distinguir qué cambios ocurren **solo al inicializar** (`prepare()`) frente a los que ocurren **en tiempo real por muestra**, y si su efecto es medible matemáticamente o audible:

1. **Cambio del factor de oversampling $M(f_s)$ entre frecuencias de muestreo:**
   - **Cuándo ocurre:** únicamente al arrancar o reiniciar el motor con otro *sample rate* (nunca en medio de una nota ni al mover una perilla).
   - **Análisis matemático:** dado que la tasa efectiva de `ADAA1` se mantiene $\ge 176.4\text{ kHz}$ en todos los casos, la atenuación inherente de tipo $\text{sinc}$ de `ADAA1` en $20\text{ kHz}$ es de apenas $\approx -0.15\text{ dB}$, y en la banda principal de guitarra ($80\text{ Hz}\text{–}5\text{ kHz}$) es **$< 0.01\text{ dB}$**. La diferencia real entre correr a $48\text{ kHz}\times 4$ (con HIIR) frente a $192\text{ kHz}\times 1$ (sin HIIR) se concentra en la respuesta de fase del filtro decimador HIIR cerca de $20\text{–}24\text{ kHz}$.
   - **Impacto auditivo:** imperceptible en un pedal aislado; sin embargo, si se encadenan muchos pedales no lineales seguidos y cada uno sobremuestrea y diezma por separado, las pequeñas diferencias de fase/transición de cada decimador sí se acumulan. Por eso, cuando un pedal tenga varias etapas seguidas (como el preamp 12AX7), subiremos la tasa **una sola vez al inicio del pedal** y bajaremos al final.

2. **Eliminación de frontera en el EQ (D2):**
   - Al usar fórmulas *matched* de Vicanek/Orfanidis en **todo** el rango de $f_s$ para EQ Paramétrico/Gráfico (y SVF-TPT en todo el rango para filtros modulables), no existe ningún punto de conmutación en $48\text{ kHz}$ ni salto entre tasas de muestreo.

3. **Frontera en tiempo real por muestra en `ADAA` ($|x_n - x_{n-1}| < \epsilon$):**
   - **Cuándo ocurre:** continuamente en cada muestra cuando la señal cruza picos planos, silencios o frecuencias graves lentas.
   - **Análisis matemático:** como `ADAA1` es suave ($C^1$ respecto al paso $\Delta x$) y con `D3` evaluamos los estados críticos en `f64`, si fijamos $\epsilon \approx 10^{-5}$ el error en la conmutación hacia la expansión de Taylor alrededor del punto medio es de orden $O(\epsilon^2) \approx 10^{-10}$ ($\approx -200\text{ dB}$). En cambio, usar `f32` puro obligaría a subir $\epsilon$ y podría mostrar escalones numéricos en gráficas de alta sensibilidad.
   - **Impacto auditivo:** **cero** ($\approx -200\text{ dB}$ está muy por debajo del piso de ruido de 24 o 32 bits).

4. **Alineación de fase al mezclar ramas paralelas (*Clean Blend*):**
   - **Caso crítico:** un *Overdrive* tipo Klon Centaur (o mezcla *dry/wet*) combina la señal directa sin distorsionar con la señal saturada. Si la rama saturada pasa por un decimador HIIR (fase no lineal arriba) o por `ADAA1` ($0.5$ muestras de retardo) y la rama limpia no se compensa, se produce un filtro peine (*comb filtering*) que cancela agudos de forma audible.
   - **Regla arquitectónica:** toda mezcla paralela debe ejecutarse **dentro** del dominio sobremuestreado (cuando haya oversampling) y compensar el retardo exacto de $0.5$ muestras de `ADAA1` en la rama limpia (detalle implementado en el Capítulo 2).

5. **Regla de verificación — *Test de invarianza entre $f_s$*:**
   - Todo pedal nuevo o actualizado en `crates/dsp` deberá pasar un test automatizado comparando su salida a $44.1$, $48$, $96$ y $192\text{ kHz}$ frente a una misma señal de prueba, exigiendo **$< 0.2\text{ dB}$** de variación en magnitud y **$< 1\text{ dB}$** en THD dentro de la banda fundamental de guitarra ($80\text{ Hz}\text{–}10\text{ kHz}$).

---

## 3. Métricas objetivas de calidad y naturalidad armónica

El objetivo declarado del proyecto es poder afinar parámetros **tanto de oído como matemáticamente** observando la estructura de armónicos y forma de onda. Para que las mediciones sean confiables y no engañen por artefactos del propio análisis espectral, se establecen las siguientes reglas basadas en el estándar **AES17**.

### 3.1 Condiciones de medición (evitar fugas espectrales y ruido fuera de banda)

1. **Muestreo coherente (*Coherent Sampling*) para banco de pruebas offline:**
   En una FFT de tamaño $N$, la frecuencia de prueba $f_{\text{in}}$ se elige exacta como:
   $$f_{\text{in}} = f_s \cdot \frac{M}{N}$$
   donde $M \in \mathbb{Z}$ es **coprimo con $N$** (habitualmente un número primo, ya que $N$ es potencia de 2). Así, tanto $f_{\text{in}}$ como todos sus armónicos caen **exactamente en el centro de un bin** (sin *spectral leakage*), separando armónicos reales de ruido o aliasing con más de $120\text{ dB}$ de rango dinámico.
2. **Ventana Blackman-Harris de 4 términos para análisis no coherente / en vivo:**
   El analizador actual de la aplicación usa una ventana **Hann**, cuyos lóbulos laterales caen apenas a unos **$-31.5\text{ dB}$**. Con una ventana Hann, el "faldón" de una fundamental fuerte oculta cualquier armónico o piso de aliasing por debajo de $-40\text{ a }-60\text{ dB}$. Una ventana **Blackman-Harris de 4 términos** (lóbulos laterales por debajo de **$-92\text{ dB}$**) permite observar armónicos finos y aliasing real en la GUI incluso cuando la frecuencia no coincide con un bin exacto.
3. **Límite de banda audible ($20\text{ kHz}$, principio AES17):**
   Cuando el sistema corre a $96\text{ kHz}$ o $192\text{ kHz}$, los productos por encima de $20\text{ kHz}$ no son audibles, pero sumarían energía si se integraran sin límite. Las métricas audibles deben evaluarse en el rango $[20\text{ Hz}, 20\text{ kHz}]$, mientras que el espectro completo sirve para auditar comportamiento ultrasónico antes de la decimación.

---

### 3.2 Las cuatro métricas principales y su interpretación musical

#### 1. THD (*Total Harmonic Distortion*)
Mide la energía total de los armónicos ($k = 2, 3, \dots, H$) respecto a la fundamental $V_1$:

$$\text{THD} = \frac{\sqrt{\sum_{k=2}^{H} V_k^2}}{V_1}, \qquad \text{THD}_{\text{dB}} = 20 \log_{10}(\text{THD})$$

- **Lectura musical:** cuantifica cuánta saturación armónica está agregando el pedal a un nivel de entrada dado (por ejemplo $-18\text{ dBFS}$ simulando pastilla *single-coil* vs. $-6\text{ dBFS}$ simulando *humbucker* caliente).

#### 2. Balance de armónicos pares vs. impares ($\text{Even/Odd}$ y $H_2 / H_3$)
Por simetría de series de Fourier:
- Una función de transferencia **impar perfecta** ($f(-x) = -f(x)$, como $\tanh(x)$ puro o dos diodos simétricos idénticos) genera **únicamente armónicos impares** ($3f_0, 5f_0, 7f_0, \dots$). Acercar la onda a un recorte simétrico fuerte empuja la forma de onda hacia una **onda cuadrada**, percibida como más hueca, comprimida o áspera si los armónicos altos no decaen rápido.
- Una función **asimétrica** ($f(-x) \ne -f(x)$, como tríodos de válvula, diodos asimétricos tipo Boss SD-1 o *bias* desplazado) introduce **armónicos pares** ($2f_0, 4f_0, \dots$), emparentados con la octava superior y asociados perceptualmente a calidez y redondez orgánicas (a costa de generar componente DC que debe eliminarse con `DcBlocker`).

Definimos dos indicadores directos:

$$R_{\text{par/impar}} = 10 \log_{10}\!\left(\frac{\sum_{k \text{ par}} V_k^2}{\sum_{k \text{ impar}\ge 3} V_k^2}\right) \text{ [dB]}, \qquad H_{2/3} = 20 \log_{10}\!\left(\frac{V_2}{V_3}\right) \text{ [dB]}$$

#### 3. Piso de aliasing / ASNR (*Aliasing-to-Signal Ratio*)
Usando una senoidal pura de frecuencia media-alta coherente (p. ej. $f_{\text{in}} \approx 4\text{–}5\text{ kHz}$ en $N = 8192$ con $M$ primo), identificamos los bins legítimos de la fundamental y sus armónicos no plegados ($k f_{\text{in}} \le \min(20\text{ kHz}, f_s/2)$). Toda la energía en el resto de bins (excluyendo el ruido de cuantización/piso) proviene del plegamiento por aliasing:

$$\text{ASNR}_{\text{dB}} = 10 \log_{10}\!\left(\frac{\sum_{b \in \text{aliasing}} |X[b]|^2}{\sum_{b \in \text{señal+armónicos}} |X[b]|^2}\right)$$

Esta métrica permite comparar con precisión numérica exacta un saturador a $1\times$ vs. `ADAA1` vs. $2\times$+`ADAA1` vs. $4\times$ HIIR.

#### 4. IMD (*Intermodulation Distortion*)
La THD con una sola nota no revela qué pasa al tocar **dos cuerdas simultáneas o un acorde** (donde las no linealidades crean frecuencias suma y diferencia inarmónicas $f_1 \pm f_2$, $2f_1 - f_2$, $2f_2 - f_1$, etc.):
- **Prueba técnica estándar (CCIF / SMPTE):** dos tonos de alta frecuencia (p. ej. $f_1 = 4\text{ kHz}, f_2 = 4.3\text{ kHz}$, o relación $19/20\text{ kHz}$ a $f_s \ge 48\text{ kHz}$) para medir el producto diferencia $f_2 - f_1$ y los laterales $2f_1 - f_2, 2f_2 - f_1$.
- **Prueba musical de guitarra:** intervalo de quinta justa (p. ej. $220\text{ Hz}$ y $330\text{ Hz}$, relación $2:3$, cuyos productos de intermodulación $f_2-f_1 = 110\text{ Hz}$ refuerzan la fundamental) frente a intervalo de tercera mayor/trítono para verificar cómo el pre-filtrado pasa-altos antes de distorsionar reduce el "embarrado" grave por intermodulación ($f_2 - f_1$).

#### 5. ESR (*Error-to-Signal Ratio*)
Para contrastar la fidelidad temporal de un modelo en tiempo real $\hat{y}[n]$ (por ejemplo, *Gray-Box* o *White-Box* WDF) frente a una referencia física de alta precisión $y_{\text{ref}}[n]$ (como un solucionador implícito sobre-muestreado a $16\times$ en `f64`), se utiliza la relación error-a-señal normalizada:

$$\text{ESR} = \frac{\sum_{n=0}^{N-1} \left(y_{\text{ref}}[n] - \hat{y}[n]\right)^2}{\sum_{n=0}^{N-1} y_{\text{ref}}[n]^2}, \qquad \text{ESR}_{\text{dB}} = 10 \log_{10}(\text{ESR})$$

- **Lectura práctica:** valores de $\text{ESR}_{\text{dB}} \le -30\text{ a }-40\text{ dB}$ indican una gran proximidad de forma de onda frente al circuito de referencia (una vez alineados retardo y ganancia DC), permitiendo cuantificar con rigor cuánto aporta un modelo *White-Box* frente a un *Gray-Box*.

### 3.3 Separación práctica: banco offline vs. analizador en vivo

- **Banco de pruebas offline (tests y *benchmarks* en `crates/dsp`):** generador de tonos coherentes + cálculo automático de THD, $H_2/H_3$, $R_{\text{par/impar}}$, IMD y $\text{ASNR}_{\text{dB}}$ en cada iteración de diseño de pedales (y detección de regresiones).
- **Analizador en vivo (GUI):** actualizar la ventana opcional a **Blackman-Harris de 4 términos** (para ver armónicos y piso hasta $-90\text{ dB}$) y, opcionalmente, añadir un pequeño generador de tono de calibración integrado para inspeccionar THD y pares/impares en pantalla en tiempo real.

---

## 4. Bibliografía y fuentes primarias

1. **Parker, J. D., Zavalishin, V., & Le Bivic, E. (2016).** *"Reducing the Aliasing of Nonlinear Waveshaping Using Continuous-Time Convolution"*. Proceedings of the 19th International Conference on Digital Audio Effects (DAFx-16), Brno, República Checa.
2. **Bilbao, S., Esqueda, F., Parker, J. D., & Välimäki, V. (2017).** *"Antiderivative Antialiasing for Memoryless Nonlinearities"*. IEEE Signal Processing Letters, vol. 24, n.º 7, pp. 1049–1053. DOI: `10.1109/LSP.2017.2675541`.
3. **Kahles, J., Esqueda, F., & Välimäki, V. (2019).** *"Oversampling for Nonlinear Waveshaping: Choosing the Right Filters"*. Journal of the Audio Engineering Society (JAES), vol. 67, n.º 6, pp. 440–449. DOI: `10.17743/jaes.2019.0012`.
4. **de Soras, L.** *HIIR — Oversampling & Hilbert Transform Polyphase IIR Half-Band Library*. Documentación técnica e implementación de referencia en filtros *all-pass* polifásicos de fase mínima.
5. **Chowdhury, J.** *"Practical Considerations for Antiderivative Anti-Aliasing"* y trabajos asociados sobre estabilidad en `f32`/`f64`, $\tanh$ estable y **ADAA-LUT** (DAFx / notas técnicas de ChowDSP).
6. **Holters, M. (2019).** *"Antiderivative Antialiasing for Stateful Systems"*. Proceedings of the 22nd International Conference on Digital Audio Effects (DAFx-19).
7. **Albertini, D., Bernardini, A., & Sarti, A. (2020).** *"Antiderivative Antialiasing in Nonlinear Wave Digital Filters"*. Proceedings of the 23rd International Conference on Digital Audio Effects (DAFx-20).
8. **Zavalishin, V.** *The Art of VA Filter Design*. Native Instruments (framework TPT / ZDF e integradores trapezoidales).
9. **Simper, A. (Cytomic).** *"Solving the continuous SVF equations using trapezoidal integration and equivalent currents"* (`SvfLinearTrapOptimised2`).
10. **Orfanidis, S. J. (1997).** *"Digital Parametric Equalizer Design with Prescribed Nyquist-Frequency Gain"*. Journal of the Audio Engineering Society (JAES).
11. **Vicanek, M. (2016).** *"Matched Second Order Digital Filters"* y *"Matched One-Pole / Two-Pole Digital Shelving Filters"*.
12. **Audio Engineering Society (AES17).** *AES standard method for digital audio engineering — Measurement of digital audio equipment* (principios de filtrado de ancho de banda audible $20\text{ kHz}$, muestreo coherente, ventanas de alto rechazo, THD e IMD).

---

## 5. Decisiones cerradas (`v1.1`)

| ID | Tema | Opciones consideradas | Decisión final |
|---|---|---|---|
| **D1** | **Estrategia base de antialiasing** | **A)** Híbrido adaptativo continuo según $f_s$ (HIIR + `ADAA1` cubriendo todo $(0,\infty)$: $8\times$ si $f_s<44.1\text{ kHz}$, $4\times$ en $44.1\text{–}48\text{ kHz}$, $2\times$ en $48\text{–}176.4\text{ kHz}$, $1\times$ desde $176.4\text{ kHz}$; `f64`/LUT solo si algún pedal exige `ADAA2`)<br>**B)** Oversampling fijo configurable en la GUI ($1\times/2\times/4\times/8\times$) | ✅ **Opción A** (partición continua de $f_s$, con selector opcional de calidad a futuro) |
| **D2** | **Arquitectura estándar de filtros** | **A)** Cytomic SVF-TPT para filtros modulables en **todas** las $f_s$ + Vicanek/Orfanidis *matched* en EQ lineal en **todas** las $f_s$ (sin conmutación en $48\text{ kHz}$)<br>**B)** Solo SVF-TPT + oversampling $2\times$ a $f_s \le 48\text{ kHz}$ | ✅ **Opción A** (sin frontera en $48\text{ kHz}$ ni oversampling en EQ) |
| **D3** | **Precisión interna (`f32` vs. `f64`)** | **A)** Buffers entre pedales en `f32`, acumuladores/estados críticos internos (SVF, ADAA, coeficientes) en `f64`<br>**B)** Todo `f32` estricto | ✅ **Opción A** |
| **D4** | **Banco de medición matemática** | **A)** Tests/benchmarks offline coherentes **+** generador de tonos integrado + ventana Blackman-Harris y métricas THD / pares-impares / aliasing en el analizador en vivo<br>**B)** Solo tests offline en `crates/dsp` | ✅ **Opción A** |
