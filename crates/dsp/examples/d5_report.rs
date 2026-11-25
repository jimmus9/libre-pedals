//! Ejecuta el experimento comparativo `D5` en `--release`, imprime las tablas Markdown
//! exactas y genera 3 gráficas SVG en `docs/research/figures/`.

use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use dsp::d5_experiment::{run_d5_experiment, D5ReportData};

#[allow(clippy::too_many_arguments)]
fn polyline_points(
    xs: &[f32],
    ys: &[f32],
    x_min: f32,
    x_max: f32,
    y_min: f32,
    y_max: f32,
    box_x: f32,
    box_y: f32,
    box_w: f32,
    box_h: f32,
) -> String {
    let mut out = String::new();
    for (&x, &y) in xs.iter().zip(ys.iter()) {
        let tx = ((x - x_min) / (x_max - x_min).max(1e-6)).clamp(0.0, 1.0);
        let ty = ((y - y_min) / (y_max - y_min).max(1e-6)).clamp(0.0, 1.0);
        let px = box_x + tx * box_w;
        let py = box_y + (1.0 - ty) * box_h;
        let _ = write!(out, "{px:.1},{py:.1} ");
    }
    out
}

fn write_waveforms_svg(path: &PathBuf, d: &D5ReportData) {
    let w = 960.0_f32;
    let h = 560.0_f32;
    let mut s = String::new();
    let _ = writeln!(
        s,
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}" font-family="Segoe UI, Arial, sans-serif">"##
    );
    let _ = writeln!(
        s,
        r##"<rect width="{w}" height="{h}" fill="#0f131a" rx="10"/>"##
    );
    let _ = writeln!(
        s,
        r##"<text x="28" y="34" fill="#e2e8f0" font-size="18" font-weight="bold">Experimento D5 — Forma de onda temporal (440 Hz, -10 dBFS, Drive 50%, 48 kHz)</text>"##
    );
    let _ = writeln!(
        s,
        r##"<text x="28" y="54" fill="#94a3b8" font-size="12">Superposición tras alineamiento de fase vs. referencia Ground Truth 16x Newton-Raphson (panel superior) y error residual (panel inferior)</text>"##
    );

    // Leyenda
    let _ = writeln!(
        s,
        r##"<line x1="28" y1="74" x2="54" y2="74" stroke="#94a3b8" stroke-width="3"/><text x="60" y="78" fill="#cbd5e1" font-size="12">Ground Truth 16x NR</text>"##
    );
    let _ = writeln!(
        s,
        r##"<line x1="210" y1="74" x2="236" y2="74" stroke="#38bdf8" stroke-width="2.5"/><text x="242" y="78" fill="#38bdf8" font-size="12">White-Box WDF + Wright ω (4x)</text>"##
    );
    let _ = writeln!(
        s,
        r##"<line x1="450" y1="74" x2="476" y2="74" stroke="#fb923c" stroke-width="2.2" stroke-dasharray="6,3"/><text x="482" y="78" fill="#fb923c" font-size="12">Gray-Box + ADAA1 (4x)</text>"##
    );

    // Panel superior: formas de onda
    let bx = 68.0_f32;
    let by1 = 94.0_f32;
    let bw = 860.0_f32;
    let bh1 = 230.0_f32;
    let _ = writeln!(
        s,
        r##"<rect x="{bx}" y="{by1}" width="{bw}" height="{bh1}" fill="#141923" stroke="#2d3748"/>"##
    );
    let t_max = *d.wave_time_ms.last().unwrap_or(&5.7);
    let amp_lim = 0.80_f32;
    for y_val in [-0.6_f32, -0.3, 0.0, 0.3, 0.6] {
        let py = by1 + (1.0 - (y_val + amp_lim) / (2.0 * amp_lim)) * bh1;
        let _ = writeln!(
            s,
            r##"<line x1="{bx}" y1="{py:.1}" x2="{}" y2="{py:.1}" stroke="#1f2937" stroke-width="1"/>"##,
            bx + bw
        );
        let _ = writeln!(
            s,
            r##"<text x="60" y="{:.1}" fill="#64748b" font-size="11" text-anchor="end">{y_val:+.1} V</text>"##,
            py + 4.0
        );
    }

    let pts_gt = polyline_points(
        &d.wave_time_ms,
        &d.wave_gt,
        0.0,
        t_max,
        -amp_lim,
        amp_lim,
        bx,
        by1,
        bw,
        bh1,
    );
    let pts_wb = polyline_points(
        &d.wave_time_ms,
        &d.wave_wb,
        0.0,
        t_max,
        -amp_lim,
        amp_lim,
        bx,
        by1,
        bw,
        bh1,
    );
    let pts_gb = polyline_points(
        &d.wave_time_ms,
        &d.wave_gb,
        0.0,
        t_max,
        -amp_lim,
        amp_lim,
        bx,
        by1,
        bw,
        bh1,
    );
    let _ = writeln!(
        s,
        r##"<polyline fill="none" stroke="#94a3b8" stroke-width="3" points="{pts_gt}"/>"##
    );
    let _ = writeln!(
        s,
        r##"<polyline fill="none" stroke="#38bdf8" stroke-width="2.2" points="{pts_wb}"/>"##
    );
    let _ = writeln!(
        s,
        r##"<polyline fill="none" stroke="#fb923c" stroke-width="2" stroke-dasharray="6,3" points="{pts_gb}"/>"##
    );

    // Panel inferior: error residual en mV
    let by2 = 354.0_f32;
    let bh2 = 165.0_f32;
    let _ = writeln!(
        s,
        r##"<rect x="{bx}" y="{by2}" width="{bw}" height="{bh2}" fill="#141923" stroke="#2d3748"/>"##
    );
    let max_err = d
        .err_wb_mv
        .iter()
        .chain(d.err_gb_mv.iter())
        .fold(5.0_f32, |m, v| m.max(v.abs()))
        * 1.15;
    for frac in [-0.75_f32, 0.0, 0.75] {
        let ev = frac * max_err;
        let py = by2 + (1.0 - (ev + max_err) / (2.0 * max_err)) * bh2;
        let _ = writeln!(
            s,
            r##"<line x1="{bx}" y1="{py:.1}" x2="{}" y2="{py:.1}" stroke="#1f2937" stroke-width="1"/>"##,
            bx + bw
        );
        let _ = writeln!(
            s,
            r##"<text x="60" y="{:.1}" fill="#64748b" font-size="11" text-anchor="end">{ev:+.1} mV</text>"##,
            py + 4.0
        );
    }
    let pts_ewb = polyline_points(
        &d.wave_time_ms,
        &d.err_wb_mv,
        0.0,
        t_max,
        -max_err,
        max_err,
        bx,
        by2,
        bw,
        bh2,
    );
    let pts_egb = polyline_points(
        &d.wave_time_ms,
        &d.err_gb_mv,
        0.0,
        t_max,
        -max_err,
        max_err,
        bx,
        by2,
        bw,
        bh2,
    );
    let _ = writeln!(
        s,
        r##"<polyline fill="none" stroke="#fb923c" stroke-width="1.8" points="{pts_egb}"/>"##
    );
    let _ = writeln!(
        s,
        r##"<polyline fill="none" stroke="#38bdf8" stroke-width="1.8" points="{pts_ewb}"/>"##
    );
    let _ = writeln!(
        s,
        r##"<text x="{}" y="542" fill="#94a3b8" font-size="12" text-anchor="middle">Tiempo (ms) — Ventana de estado estacionario (0 .. {t_max:.1} ms)</text>"##,
        bx + bw * 0.5
    );
    let _ = writeln!(s, "</svg>");
    fs::write(path, s).unwrap();
}

fn write_aliasing_svg(path: &PathBuf, d: &D5ReportData) {
    let w = 960.0_f32;
    let h = 540.0_f32;
    let mut s = String::new();
    let _ = writeln!(
        s,
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}" font-family="Segoe UI, Arial, sans-serif">"##
    );
    let _ = writeln!(
        s,
        r##"<rect width="{w}" height="{h}" fill="#0f131a" rx="10"/>"##
    );
    let _ = writeln!(
        s,
        r##"<text x="28" y="34" fill="#e2e8f0" font-size="18" font-weight="bold">Experimento D5 — Espectro Blackman-Harris 4 para E2 (Seno 4.5 kHz, -6 dBFS, Drive 75%)</text>"##
    );
    let _ = writeln!(
        s,
        r##"<text x="28" y="54" fill="#94a3b8" font-size="12">Arriba: 48 kHz (M=4x) vs. Abajo: 192 kHz (M=1x directo) mostrando cómo ADAA1 atenúa alias de orden alto a 1x</text>"##
    );

    let bx = 68.0_f32;
    let bw = 860.0_f32;
    let bh = 185.0_f32;

    let panels = [
        (
            86.0_f32,
            "48 kHz (Sobremuestreo adaptativo 4x)",
            &d.spec_freqs_48k,
            Some(&d.spec_gt_48k),
            &d.spec_wb_48k,
            &d.spec_gb_48k,
        ),
        (
            312.0_f32,
            "192 kHz (Procesado directo 1x sin sobremuestreo)",
            &d.spec_freqs_192k,
            None,
            &d.spec_wb_192k,
            &d.spec_gb_192k,
        ),
    ];

    let max_f = 24_000.0_f32;
    for (by, title, freqs, gt_opt, wb, gb) in panels {
        let _ = writeln!(
            s,
            r##"<text x="{bx}" y="{:.1}" fill="#cbd5e1" font-size="12" font-weight="bold">{title}</text>"##,
            by - 6.0
        );
        let _ = writeln!(
            s,
            r##"<rect x="{bx}" y="{by}" width="{bw}" height="{bh}" fill="#141923" stroke="#2d3748"/>"##
        );
        for db in [0.0_f32, -25.0, -50.0, -75.0, -100.0] {
            let py = by + (1.0 - (db + 105.0) / 110.0) * bh;
            let _ = writeln!(
                s,
                r##"<line x1="{bx}" y1="{py:.1}" x2="{}" y2="{py:.1}" stroke="#1f2937" stroke-width="1"/>"##,
                bx + bw
            );
            let _ = writeln!(
                s,
                r##"<text x="60" y="{:.1}" fill="#64748b" font-size="10" text-anchor="end">{db:.0} dB</text>"##,
                py + 3.0
            );
        }
        for fk in [4500.0_f32, 9000.0, 13500.0, 18000.0, 22500.0] {
            let px = bx + (fk / max_f) * bw;
            let _ = writeln!(
                s,
                r##"<line x1="{px:.1}" y1="{by}" x2="{px:.1}" y2="{}" stroke="#1f2937" stroke-width="1"/>"##,
                by + bh
            );
            let _ = writeln!(
                s,
                r##"<text x="{px:.1}" y="{:.1}" fill="#64748b" font-size="10" text-anchor="middle">{:.1}k</text>"##,
                by + bh + 13.0,
                fk / 1000.0
            );
        }

        // Submuestreamos bins <= 24 kHz a máx ~600 puntos tomando máximo local por píxel horizontal.
        let downsample_spec = |spec: &[f32]| -> (Vec<f32>, Vec<f32>) {
            let cols = 540_usize;
            let mut fx = Vec::with_capacity(cols);
            let mut vy = Vec::with_capacity(cols);
            let bin_hz = freqs.get(1).copied().unwrap_or(6.0);
            for c in 0..cols {
                let f0 = (c as f32 / cols as f32) * max_f;
                let f1 = ((c + 1) as f32 / cols as f32) * max_f;
                let k0 = (f0 / bin_hz).floor() as usize;
                let k1 = ((f1 / bin_hz).ceil() as usize).min(spec.len() - 1).max(k0);
                let mut peak = -120.0_f32;
                for &v in &spec[k0..=k1] {
                    if v > peak {
                        peak = v;
                    }
                }
                fx.push(0.5 * (f0 + f1));
                vy.push(peak.clamp(-105.0, 5.0));
            }
            (fx, vy)
        };

        if let Some(gt) = gt_opt {
            let (fx, vy) = downsample_spec(gt);
            let pts = polyline_points(&fx, &vy, 0.0, max_f, -105.0, 5.0, bx, by, bw, bh);
            let _ = writeln!(
                s,
                r##"<polyline fill="none" stroke="#64748b" stroke-width="1.6" points="{pts}"/>"##
            );
        }
        let (fx_w, vy_w) = downsample_spec(wb);
        let pts_w = polyline_points(&fx_w, &vy_w, 0.0, max_f, -105.0, 5.0, bx, by, bw, bh);
        let _ = writeln!(
            s,
            r##"<polyline fill="none" stroke="#38bdf8" stroke-width="1.5" points="{pts_w}"/>"##
        );

        let (fx_g, vy_g) = downsample_spec(gb);
        let pts_g = polyline_points(&fx_g, &vy_g, 0.0, max_f, -105.0, 5.0, bx, by, bw, bh);
        let _ = writeln!(
            s,
            r##"<polyline fill="none" stroke="#fb923c" stroke-width="1.5" opacity="0.9" points="{pts_g}"/>"##
        );
    }

    let _ = writeln!(
        s,
        r##"<line x1="28" y1="522" x2="54" y2="522" stroke="#64748b" stroke-width="2"/><text x="60" y="526" fill="#94a3b8" font-size="12">Ground Truth 16x</text>"##
    );
    let _ = writeln!(
        s,
        r##"<line x1="200" y1="522" x2="226" y2="522" stroke="#38bdf8" stroke-width="2"/><text x="232" y="526" fill="#38bdf8" font-size="12">White-Box WDF + Wright ω</text>"##
    );
    let _ = writeln!(
        s,
        r##"<line x1="420" y1="522" x2="446" y2="522" stroke="#fb923c" stroke-width="2"/><text x="452" y="526" fill="#fb923c" font-size="12">Gray-Box + ADAA1</text>"##
    );
    let _ = writeln!(s, "</svg>");
    fs::write(path, s).unwrap();
}

fn write_sweep_svg(path: &PathBuf, d: &D5ReportData) {
    let w = 960.0_f32;
    let h = 460.0_f32;
    let mut s = String::new();
    let _ = writeln!(
        s,
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}" font-family="Segoe UI, Arial, sans-serif">"##
    );
    let _ = writeln!(
        s,
        r##"<rect width="{w}" height="{h}" fill="#0f131a" rx="10"/>"##
    );
    let _ = writeln!(
        s,
        r##"<text x="28" y="34" fill="#e2e8f0" font-size="18" font-weight="bold">Experimento D5 — Barrido de Amplitud E1 (440 Hz, -40..0 dBFS @ 48 kHz)</text>"##
    );
    let _ = writeln!(
        s,
        r##"<text x="28" y="54" fill="#94a3b8" font-size="12">Izquierda: THD (%) vs. Nivel de entrada (Drive 50% y 100%) | Derecha: Fidelidad ESR (dB) frente a Ground Truth 16x</text>"##
    );

    // Panel Izquierdo: THD%
    let bx1 = 68.0_f32;
    let by = 92.0_f32;
    let bw = 390.0_f32;
    let bh = 290.0_f32;
    let _ = writeln!(
        s,
        r##"<rect x="{bx1}" y="{by}" width="{bw}" height="{bh}" fill="#141923" stroke="#2d3748"/>"##
    );
    let max_thd = 35.0_f32;
    for t_val in [0.0_f32, 10.0, 20.0, 30.0] {
        let py = by + (1.0 - t_val / max_thd) * bh;
        let _ = writeln!(
            s,
            r##"<line x1="{bx1}" y1="{py:.1}" x2="{}" y2="{py:.1}" stroke="#1f2937"/>"##,
            bx1 + bw
        );
        let _ = writeln!(
            s,
            r##"<text x="60" y="{:.1}" fill="#64748b" font-size="11" text-anchor="end">{t_val:.0}%</text>"##,
            py + 4.0
        );
    }

    // Panel Derecho: ESR (dB)
    let bx2 = 536.0_f32;
    let _ = writeln!(
        s,
        r##"<rect x="{bx2}" y="{by}" width="{bw}" height="{bh}" fill="#141923" stroke="#2d3748"/>"##
    );
    for e_val in [-20.0_f32, -40.0, -60.0, -80.0, -100.0] {
        let py = by + (1.0 - (e_val + 105.0) / 95.0) * bh;
        let _ = writeln!(
            s,
            r##"<line x1="{bx2}" y1="{py:.1}" x2="{}" y2="{py:.1}" stroke="#1f2937"/>"##,
            bx2 + bw
        );
        let _ = writeln!(
            s,
            r##"<text x="528" y="{:.1}" fill="#64748b" font-size="11" text-anchor="end">{e_val:.0} dB</text>"##,
            py + 4.0
        );
    }

    for &drv in &[25.0_f32, 50.0, 100.0] {
        let pts: Vec<_> = d
            .sweep
            .iter()
            .filter(|p| (p.drive_pct - drv).abs() < 0.1)
            .collect();
        let lvls: Vec<f32> = pts.iter().map(|p| p.level_db).collect();
        let gt_thds: Vec<f32> = pts.iter().map(|p| p.gt_thd_pct).collect();
        let wb_thds: Vec<f32> = pts.iter().map(|p| p.wb_thd_pct).collect();
        let gb_thds: Vec<f32> = pts.iter().map(|p| p.gb_thd_pct).collect();
        let wb_esrs: Vec<f32> = pts
            .iter()
            .map(|p| p.wb_esr_db.clamp(-105.0, -10.0))
            .collect();
        let gb_esrs: Vec<f32> = pts
            .iter()
            .map(|p| p.gb_esr_db.clamp(-105.0, -10.0))
            .collect();

        let dash = if drv < 40.0 {
            "2,3"
        } else if drv < 80.0 {
            ""
        } else {
            "8,3"
        };
        let p_gt = polyline_points(&lvls, &gt_thds, -40.0, 0.0, 0.0, max_thd, bx1, by, bw, bh);
        let p_wb = polyline_points(&lvls, &wb_thds, -40.0, 0.0, 0.0, max_thd, bx1, by, bw, bh);
        let p_gb = polyline_points(&lvls, &gb_thds, -40.0, 0.0, 0.0, max_thd, bx1, by, bw, bh);
        let _ = writeln!(
            s,
            r##"<polyline fill="none" stroke="#94a3b8" stroke-width="2" stroke-dasharray="{dash}" points="{p_gt}"/>"##
        );
        let _ = writeln!(
            s,
            r##"<polyline fill="none" stroke="#38bdf8" stroke-width="2" stroke-dasharray="{dash}" points="{p_wb}"/>"##
        );
        let _ = writeln!(
            s,
            r##"<polyline fill="none" stroke="#fb923c" stroke-width="2" stroke-dasharray="{dash}" points="{p_gb}"/>"##
        );

        let pe_wb = polyline_points(&lvls, &wb_esrs, -40.0, 0.0, -105.0, -10.0, bx2, by, bw, bh);
        let pe_gb = polyline_points(&lvls, &gb_esrs, -40.0, 0.0, -105.0, -10.0, bx2, by, bw, bh);
        let _ = writeln!(
            s,
            r##"<polyline fill="none" stroke="#38bdf8" stroke-width="2" stroke-dasharray="{dash}" points="{pe_wb}"/>"##
        );
        let _ = writeln!(
            s,
            r##"<polyline fill="none" stroke="#fb923c" stroke-width="2" stroke-dasharray="{dash}" points="{pe_gb}"/>"##
        );
    }

    for bx in [bx1, bx2] {
        for lvl in [-40.0_f32, -30.0, -20.0, -10.0, 0.0] {
            let px = bx + ((lvl + 40.0) / 40.0) * bw;
            let _ = writeln!(
                s,
                r##"<text x="{px:.1}" y="{:.1}" fill="#64748b" font-size="11" text-anchor="middle">{lvl:.0}</text>"##,
                by + bh + 16.0
            );
        }
    }
    let _ = writeln!(
        s,
        r##"<text x="{}" y="425" fill="#cbd5e1" font-size="12" text-anchor="middle">Nivel de entrada (dBFS) — Drive 25% (punteado), 50% (continuo), 100% (trazos)</text>"##,
        w * 0.5
    );
    let _ = writeln!(
        s,
        r##"<text x="28" y="446" fill="#94a3b8" font-size="12">Gris: Ground Truth 16x | Azul: White-Box WDF+Wright ω | Naranja: Gray-Box+ADAA1 (menor ESR = más fiel al circuito)</text>"##
    );
    let _ = writeln!(s, "</svg>");
    fs::write(path, s).unwrap();
}

fn main() {
    let report = run_d5_experiment();

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let fig_dir = repo_root.join("docs/research/figures");
    fs::create_dir_all(&fig_dir).expect("crear docs/research/figures");

    write_waveforms_svg(&fig_dir.join("d5_waveforms_and_error.svg"), &report);
    write_aliasing_svg(&fig_dir.join("d5_aliasing_spectrum.svg"), &report);
    write_sweep_svg(&fig_dir.join("d5_sweep_thd_esr.svg"), &report);

    println!("| Modelo / Tasa | ESR vs. GT | THD (440 Hz) | H2 / H3 (dB) | IMD 5.ª / 3.ª (dB) | ASNR (4.5 kHz) | ESR Transitorio | CPU (ns/m) | Latencia (m) |");
    println!("|---|---|---|---|---|---|---|---|---|");
    for r in &report.rows {
        let esr_str = if r.esr_db_440 <= -135.0 {
            "Ref. (0 dB)".to_string()
        } else {
            format!("{:.1} dB", r.esr_db_440)
        };
        let tr_str = if r.esr_db_transient <= -135.0 {
            "Ref.".to_string()
        } else {
            format!("{:.1} dB", r.esr_db_transient)
        };
        println!(
            "| **{} ({} kHz)** | {} | {:.2}% | {:.1} / {:.1} | {:.1} / {:.1} | {:.1} dB | {} | {:.1} | {:.2} |",
            r.label,
            r.sample_rate / 1000,
            esr_str,
            r.thd_percent_440,
            r.h2_db,
            r.h3_db,
            r.imd_fifth_db,
            r.imd_third_db,
            r.asnr_db_4500,
            tr_str,
            r.ns_per_sample,
            r.latency_samples
        );
    }

    println!("\n--- Barrido E1 (Drive 50% @ 48 kHz) ---");
    for p in report
        .sweep
        .iter()
        .filter(|p| (p.drive_pct - 50.0).abs() < 0.1)
    {
        println!(
            "lvl={:>5.1} dBFS | GT THD={:.2}% | WB THD={:.2}% (ESR={:.1} dB) | GB THD={:.2}% (ESR={:.1} dB)",
            p.level_db, p.gt_thd_pct, p.wb_thd_pct, p.wb_esr_db, p.gb_thd_pct, p.gb_esr_db
        );
    }
}
