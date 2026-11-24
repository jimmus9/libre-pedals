# Registro Bibliográfico de Investigación DSP — `libre-pedals`

Registro acumulativo de artículos científicos, estándares y referencias técnicas revisadas durante la investigación de arquitectura DSP para `libre-pedals`. Este índice servirá como base bibliográfica para redactar el documento formal final del proyecto.

---

## Capítulo 1 — Fundamentos DSP (Antialiasing, Filtros y Métricas)

| ID | Referencia | Publicación / Tipo | Aporte específico al proyecto |
|---|---|---|---|
| `[Parker2016]` | Julian D. Parker, Vadim Zavalishin, Efflam Le Bivic (2016). *"Reducing the Aliasing of Nonlinear Waveshaping Using Continuous-Time Convolution"*. | DAFx-16 (19th International Conference on Digital Audio Effects, Brno) | Base conceptual de ADAA: reconstrucción continua por partes y convolución analítica antes de remuestrear para atenuar *aliasing* en *waveshaping* sin oversampling extremo. |
| `[Bilbao2017]` | Stefan Bilbao, Fabián Esqueda, Julian D. Parker, Vesa Välimäki (2017). *"Antiderivative Antialiasing for Memoryless Nonlinearities"*. DOI: `10.1109/LSP.2017.2675541` | IEEE Signal Processing Letters, Vol. 24, No. 7, pp. 1049–1053 | Formalización de ADAA de 1.º, 2.º y orden superior mediante antiderivadas sucesivas y diferencias finitas; relación SNR vs. orden y costo computacional. |
| `[Kahles2019]` | Julen Kahles, Fabián Esqueda, Vesa Välimäki (2019). *"Oversampling for Nonlinear Waveshaping: Choosing the Right Filters"*. DOI: `10.17743/jaes.2019.0012` | Journal of the Audio Engineering Society (JAES), Vol. 67, No. 6, pp. 440–449 | Criterio cuantitativo de filtros de *upsampling* (interpolación lineal + *high-shelf*) y *decimation* (CIC con *leakage* + *high-shelf*, IIR elípticos, FIR) evaluados con *noise-to-mask ratio* ponderado A en recorte duro. |
| `[Holters2019]` | Martin Holters (2019). *"Antiderivative Antialiasing for Stateful Systems"*. | DAFx-19 (22nd International Conference on Digital Audio Effects) | Extensión de ADAA desde no linealidades sin memoria hacia sistemas dinámicos con estado interno y realimentación sin romper estabilidad ni afinación. |
| `[Albertini2020]` | Davide Albertini, Alberto Bernardini, Augusto Sarti (2020). *"Antiderivative Antialiasing in Nonlinear Wave Digital Filters"*. | DAFx-20 (23rd International Conference on Digital Audio Effects) | Integración modular de ADAA de orden arbitrario dentro de *Wave Digital Filters* (WDF) para circuitos no lineales de pedales y filtros analógicos. |
| `[deSorasHIIR]` | Laurent de Soras. *HIIR — Oversampling and Hilbert Transform Library* (Licencia WTFPL). | Biblioteca DSP / Diseño polifásico IIR | Implementación de referencia para *oversampling* ×2 encadenable (×2, ×4, ×8) mediante dos ramas *all-pass* IIR *half-band* de fase mínima, alta atenuación en banda de parada y muy baja latencia/CPU. |
| `[ChowdhuryADAA]` | Jatin Chowdhury. *Practical Considerations for Antiderivative Anti-Aliasing*. | Nota técnica / artículo de ingeniería DSP | Análisis del mal condicionamiento cuando $\|x_n - x_{n-1}\| \to 0$, umbrales de transición a serie de Taylor, vulnerabilidad de `f32` frente a `f64` en ADAA2, evaluación estable de $\log(\cosh(x))$ y enfoque ADAA-LUT para evitar $\text{Li}_2$. |
| `[ZavalishinVA]` | Vadim Zavalishin. *The Art of VA Filter Design* (Native Instruments). | Monografía técnica (Virtual Analog DSP) | Marco **TPT** (*Topology-Preserving Transform*) y **ZDF** (*Zero-Delay Feedback*) mediante integración trapezoidal para filtros estables bajo modulación rápida. |
| `[SimperSVF]` | Andrew Simper (Cytomic). *"Solving the continuous SVF equations using trapezoidal integration and equivalent currents"* (`SvfLinearTrapOptimised2`). | Whitepaper técnico (Cytomic) | Ecuaciones cerradas en forma optimizada (`g`, `k`, `a1`, `a2`, `a3`, `ic1eq`, `ic2eq`) para SVF lineal trapezoidal con bajo ruido numérico y salidas simultáneas LP/BP/HP/Notch. |
| `[Orfanidis1997]` | Sophocles J. Orfanidis (1997). *"Digital Parametric Equalizer Design with Prescribed Nyquist-Frequency Gain"*. | Journal of the Audio Engineering Society (JAES) | Corrección del *cramping* de la transformada bilineal prescribiendo exactamente la ganancia en Nyquist en filtros paramétricos digitales. |
| `[Vicanek2016]` | Martin Vicanek (2016). *"Matched Second Order Digital Filters"* y *"Matched One-Pole / Two-Pole Digital Shelving Filters"*. | Notas técnicas (vicanek.de) | Coeficientes biquad y *shelving* en forma cerrada que ajustan la respuesta en magnitud hasta la frecuencia de Nyquist sin recurrir a *oversampling* en EQ. |
| `[AES17]` | Audio Engineering Society. *AES17 — Standard Method for Digital Audio Engineering*. | Estándar de medición de audio digital | Metodología de medición de THD, THD+N e IMD, filtrado de banda audible, muestreo coherente ($f_{\text{in}} = f_s \cdot M / N$), ventanas Blackman-Harris de alta supresión de lóbulos laterales y separación de armónicos pares vs. impares. |

---

## Capítulo 2 — Saturación, Overdrive y Preamplificación a Válvulas

| ID | Referencia | Publicación / Tipo | Aporte específico al proyecto |
|---|---|---|---|
| `[Pakarinen2009]` | Jyri Pakarinen, David T. Yeh (2009). *"A Review of Digital Techniques for Modeling Vacuum-Tube Guitar Amplifiers"*. | Computer Music Journal, Vol. 33, No. 2, pp. 85–100 (MIT Press) | Taxonomía *Black-Box / Gray-Box / White-Box* y análisis físico de asimetría, *grid current* y etapas de amplificación a válvulas. |
| `[Yeh2010]` | David T. Yeh, Jonathan S. Abel, Julius O. Smith III (2010). *"Automated Physical Modeling of Nonlinear Audio Circuits for Real-Time Audio Effects—Part I & Part II"*. | IEEE Trans. on Audio, Speech, and Language Processing (CCRMA) | Formulación física de ODEs rígidas en circuitos de diodos (TS-808), BJT y tríodos; referencia base para el experimento `D5`. |
| `[DAngelo2019]` | Stefano D'Angelo, Leonardo Gabrielli, Luca Turchet (2019). *"Fast Approximation of the Lambert W Function for Virtual Analog Modelling"*. | DAFx-19 (22nd International Conference on Digital Audio Effects) | Solución cerrada sin exponenciales en tasa de audio para diodos en árboles WDF mediante la función Omega de Wright $\omega(z) = W(e^z)$. |
| `[Koren1996]` | Norman Koren (1996). *"Improved Vacuum-Tube Models for Spice Simulations"*. | Nota técnica / modelo fenomenológico de válvulas | Ecuaciones continuas de placa ($E_1, I_p$) y rejilla para modelar el tríodo 12AX7 sin discontinuidades abruptas. |
| `[ChowCentaur]` | Jatin Chowdhury (ChowDSP). *ChowCentaur — Klon Centaur Circuit Analysis & VA Modeling*. | Documentación técnica e implementación abierta | Análisis del potenciómetro *dual-gang* (*Clean Blend* paralelo), diodos de germanio 1N34A y modelado híbrido WDF/RNN. |
| `[PedalCircuits]` | Análisis esquemáticos de Ibanez TS-808 (*ElectroSmash*), Boss SD-1, MXR Micro Amp y Xotic EP Booster. | Documentación de ingeniería de pedales | Valores reales de la red de realimentación TS-808 ($4.7\text{ k}\Omega / 0.047\,\mu\text{F} \approx 720\text{ Hz}$, $51\text{ pF}$), asimetría 2:1, etapas Op-Amp/JFET y *headroom* $9/18\text{ V}$. |

---

## Capítulo 3 — Dinámica (Noise Gate, Compresor Óptico, Limitador Brickwall y Tape/Soft Clipper)

| ID | Referencia | Publicación / Tipo | Aporte específico al proyecto |
|---|---|---|---|
| `[Giannoulis2012]` | Dimitrios Giannoulis, Michael Massberg, Joshua D. Reiss (2012). *"Digital Dynamic Range Compressor Design—A Tutorial and Analysis"*. | Journal of the Audio Engineering Society (JAES), Vol. 60, No. 6, pp. 399–408 | Ecuación estándar de *soft knee*, comparación *feed-forward / feed-back* y análisis de detectores ramificados vs. desacoplados. |
| `[MooreLA2A]` | Austin Moore, Rupert Till, Jonathan P. Wakefield (JAES / Univ. of Huddersfield). | Journal of the Audio Engineering Society / investigaciones empíricas | Caracterización objetiva y perceptual del compresor óptico Teletronix LA-2A (celda T4B EL/CdS, relajación en dos etapas y memoria de exposición). |
| `[ITUBS1770]` | International Telecommunication Union. *ITU-R BS.1770 (Anexo 2) — True-Peak Audio Level*. | Estándar internacional de radiodifusión | Detección de picos inter-muestra (*ISPs*) mediante sobremuestreo $\ge 4\times$ para limitadores *brickwall* sin *overshoot*. |
| `[Chowdhury2019]` | Jatin Chowdhury (2019). *"Real-time Physical Modelling for Analog Tape Machines"*. | DAFx-19 (Birmingham) | Modelo físico de cinta magnética mediante histéresis de Jiles-Atherton y pérdidas dependientes de frecuencia para *Tape Limiter*. |
| `[GateLookahead]` | Literatura de procesado dinámico en tiempo real (cola monótona $\mathcal{O}(1)$ e histéresis de doble umbral). | Algoritmos estándar de audio DSP | Máximo móvil en ventana de *lookahead* con memoria fija, prevención de *chatter* y filtrado *sidechain* selectivo. |

---

## Capítulo 4 — EQ, Filtros Dinámicos, Simulación de Gabinete (IR), Modulación y Tiempo

| ID | Referencia | Publicación / Tipo | Aporte específico al proyecto |
|---|---|---|---|
| `[Gardner1995]` | William G. Gardner (1995). *"Efficient Convolution without Input/Output Delay"*. | Journal of the Audio Engineering Society (JAES), Vol. 43, No. 3, pp. 127–136 | Convolución particionada híbrida de latencia cero (cabeza directa en el tiempo + cola FFT/UPOLS) para el simulador de gabinete (*Cabinet IR*). |
| `[ValimakiLiski2017]` | Vesa Välimäki, Juho Liski (2017). *"Accurate Cascade Graphic Equalizer"* y *"The Quest for the Best Graphic Equalizer"*. | IEEE Signal Processing Letters (Vol. 24, No. 2) / DAFx-17 | Diseño preciso ($\pm 1\text{ dB}$) de ecualizadores gráficos en cascada mediante matriz de interacción entre bandas vecinas para el EQ de 7 bandas. |
| `[HoltersCryBaby]` | Martin Holters, Udo Zölzer. *"Physical Modelling of a Wah-Wah Effect Pedal as a Case Study for Application of the Nodal DK Method to Circuits with Variable Parts"*. | DAFx (Helmut Schmidt University) | Modelado de la red resonante LC y variación acoplada de corte/$Q$ del Dunlop CryBaby GCB-95 como base de calibración del Wah / Auto-Wah. |
| `[Laakso1996]` | T. I. Laakso, V. Välimäki, M. Karjalainen, U. K. Laine (1996). *"Splitting the Unit Delay: Tools for Fractional Delay Filter Design"*. | IEEE Signal Processing Magazine, Vol. 13, No. 1, pp. 30–60 | Taxonomía de retardos fraccionales (FIR Lagrange/Hermite vs. Thiran *all-pass*); justifica Hermite cúbica en líneas moduladas por LFO. |
| `[Raffel2010]` | Colin Raffel, Julius O. Smith III (2010). *"Practical Modeling of Bucket-Brigade Device Circuits"*. | DAFx-10 (Graz, CCRMA Stanford) | Modelado integral de líneas BBD (filtros anti-aliasing/reconstrucción y compansión) para Chorus y Delay analógico. |
| `[Kiiski2016]` | Roope Kiiski, Fabián Esqueda, Vesa Välimäki (2016). *"Time-Variant Gray-Box Modeling of a Phaser Pedal"*. | DAFx-16 (Brno) | Modelado *Gray-Box* de phaser tipo MXR Phase 90 mediante cascada de filtros *all-pass* de 1.er orden modulados exponencialmente por LFO. |
| `[Dattorro1997]` | Jon Dattorro (1997). *"Effect Design, Part 1: Reverberator and Other Filters"*. | Journal of the Audio Engineering Society (JAES), Vol. 45, No. 9 | Arquitectura estándar de reverb *Plate* (difusores de entrada + tanque figura-ocho modulado con *damping*). |
| `[ValimakiSpring2010]` | Vesa Välimäki, Julian D. Parker, Jonathan S. Abel (2010). *"Parametric Spring Reverberation Effect"*. (y Abel et al., AES 2006). | Journal of the Audio Engineering Society (JAES), Vol. 58, No. 7/8, pp. 547–562 | Cadena dispersiva de filtros *all-pass* por debajo de la frecuencia de transición para sintetizar el *"chirp"* característico de *Spring Reverb*. |
