//! The Peek mark as plain pixels. Kept free of other crates so build.rs
//! can render the same mark into the executable's icon.

/// RGBA pixels of the mark at `size`×`size`, antialiased with signed distances.
pub fn rgba(size: u32) -> Vec<u8> {
    let s = size as f32;
    let mut out = vec![0u8; (size * size * 4) as usize];
    let c = s / 2.0;
    // Eye: a lens made of two circles, chord 0.64 s wide and 0.40 s tall.
    let (half_w, half_h) = (0.32 * s, 0.20 * s);
    let r = (half_w * half_w + half_h * half_h) / (2.0 * half_h);
    let (pupil_c, pupil_r) = ((c + 0.05 * s, c - 0.01 * s), 0.115 * s);
    for y in 0..size {
        for x in 0..size {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            // Rounded square.
            let q = (
                (px - c).abs() - (c - 0.22 * s),
                (py - c).abs() - (c - 0.22 * s),
            );
            let outside = (q.0.max(0.0).powi(2) + q.1.max(0.0).powi(2)).sqrt();
            let d_tile = outside + q.0.max(q.1).min(0.0) - 0.22 * s + 0.5;
            let a_tile = (0.5 - d_tile).clamp(0.0, 1.0);
            if a_tile <= 0.0 {
                continue;
            }
            // Diagonal gradient, light top-left to deep bottom-right.
            let t = ((px + py) / (2.0 * s)).clamp(0.0, 1.0);
            let mix = |a: f32, b: f32| a + (b - a) * t;
            let mut col = [mix(124.0, 34.0), mix(178.0, 96.0), mix(255.0, 214.0)];
            let d1 = ((px - c).powi(2) + (py - (c + r - half_h)).powi(2)).sqrt() - r;
            let d2 = ((px - c).powi(2) + (py - (c - r + half_h)).powi(2)).sqrt() - r;
            let a_eye = (0.5 - d1.max(d2)).clamp(0.0, 1.0);
            col = blend(col, [255.0, 255.0, 255.0], a_eye);
            let dp = ((px - pupil_c.0).powi(2) + (py - pupil_c.1).powi(2)).sqrt() - pupil_r;
            col = blend(col, [22.0, 44.0, 92.0], (0.5 - dp).clamp(0.0, 1.0) * a_eye);
            let dh = ((px - pupil_c.0 - 0.04 * s).powi(2) + (py - pupil_c.1 + 0.04 * s).powi(2))
                .sqrt()
                - 0.035 * s;
            col = blend(
                col,
                [255.0, 255.0, 255.0],
                (0.5 - dh).clamp(0.0, 1.0) * a_eye,
            );
            let i = ((y * size + x) * 4) as usize;
            out[i..i + 4].copy_from_slice(&[
                col[0] as u8,
                col[1] as u8,
                col[2] as u8,
                (a_tile * 255.0) as u8,
            ]);
        }
    }
    out
}

fn blend(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}
