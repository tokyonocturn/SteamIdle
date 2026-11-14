//! Programmatically drawn app icon (RGBA, anti-aliased by 4x supersampling).
//! Keeping it in code avoids binary asset parsing and guarantees the tray icon
//! matches the executable icon baked into `assets/steamidle.ico`.

const BG: [f32; 3] = [27.0, 40.0, 56.0];
const RING: [f32; 3] = [102.0, 192.0, 244.0];
const PLAY: [f32; 3] = [240.0, 248.0, 255.0];

/// Returns `size * size * 4` unmultiplied RGBA bytes.
pub fn render(size: u32) -> Vec<u8> {
    let size_f = size as f32;
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    let center = size_f / 2.0;
    let ring_radius = size_f * 0.31;
    let ring_thickness = size_f * 0.075;
    let margin = size_f * 0.055;
    let corner = size_f * 0.22;

    let triangle = [
        (center - size_f * 0.065, center - size_f * 0.135),
        (center - size_f * 0.065, center + size_f * 0.135),
        (center + size_f * 0.155, center),
    ];

    const SAMPLES: u32 = 4;
    let total_samples = (SAMPLES * SAMPLES) as f32;

    for y in 0..size {
        for x in 0..size {
            let mut sums = [0.0f32; 3];
            let mut covered = 0.0f32;
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let px = x as f32 + (sx as f32 + 0.5) / SAMPLES as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / SAMPLES as f32;
                    let mut color = None;
                    if in_rounded_rect(px, py, margin, size_f - margin, corner) {
                        color = Some(BG);
                        let distance = ((px - center).powi(2) + (py - center).powi(2)).sqrt();
                        if (distance - ring_radius).abs() <= ring_thickness / 2.0 {
                            color = Some(RING);
                        }
                    }
                    if in_triangle(px, py, triangle) {
                        color = Some(PLAY);
                    }
                    if let Some(color) = color {
                        sums[0] += color[0];
                        sums[1] += color[1];
                        sums[2] += color[2];
                        covered += 1.0;
                    }
                }
            }
            if covered == 0.0 {
                pixels.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                pixels.push((sums[0] / covered).round() as u8);
                pixels.push((sums[1] / covered).round() as u8);
                pixels.push((sums[2] / covered).round() as u8);
                pixels.push((255.0 * covered / total_samples).round() as u8);
            }
        }
    }
    pixels
}

fn in_rounded_rect(px: f32, py: f32, min: f32, max: f32, radius: f32) -> bool {
    if px < min || px > max || py < min || py > max {
        return false;
    }
    let nearest_x = px.clamp(min + radius, max - radius);
    let nearest_y = py.clamp(min + radius, max - radius);
    (px - nearest_x).powi(2) + (py - nearest_y).powi(2) <= radius * radius
}

fn in_triangle(px: f32, py: f32, points: [(f32, f32); 3]) -> bool {
    let sign = |a: (f32, f32), b: (f32, f32), c: (f32, f32)| {
        (a.0 - c.0) * (b.1 - c.1) - (b.0 - c.0) * (a.1 - c.1)
    };
    let d1 = sign((px, py), points[0], points[1]);
    let d2 = sign((px, py), points[1], points[2]);
    let d3 = sign((px, py), points[2], points[0]);
    let has_negative = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_positive = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_negative && has_positive)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_has_expected_shape_and_transparency() {
        let size = 32;
        let pixels = render(size);
        assert_eq!(pixels.len(), (size * size * 4) as usize);
        // Corners are transparent, the middle is opaque.
        assert_eq!(pixels[3], 0);
        let center = ((size / 2) * size + size / 2) as usize * 4;
        assert!(pixels[center + 3] > 200);
    }
}
