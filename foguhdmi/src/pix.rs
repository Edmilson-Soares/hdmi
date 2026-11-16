//! Primitivas de desenho com anti-aliasing sobre um buffer 0x00RRGGBB
//! (o mesmo layout de uma DIB section 32 bpp do Windows).

/// Cor com transparência. No lado C/Fogu a cor é um i64:
/// - `0xRRGGBB` (0..=0xFFFFFF): opaca;
/// - `0x1_AARRGGBB` (bit 32 ligado): com alfa AA.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Paint {
    pub rgb: u32,
    pub a: u32,
}

impl Paint {
    pub fn from_i64(c: i64) -> Paint {
        if c >= 0x1_0000_0000 {
            Paint { rgb: (c & 0xFF_FFFF) as u32, a: ((c >> 24) & 0xFF) as u32 }
        } else {
            Paint { rgb: (c & 0xFF_FFFF) as u32, a: 255 }
        }
    }
}

#[inline]
pub fn blend(d: u32, c: u32, a: u32) -> u32 {
    if a >= 255 {
        return c;
    }
    if a == 0 {
        return d;
    }
    let inv = 255 - a;
    let rb = (((c & 0xFF00FF) * a + (d & 0xFF00FF) * inv) >> 8) & 0xFF00FF;
    let g = (((c & 0x00FF00) * a + (d & 0x00FF00) * inv) >> 8) & 0x00FF00;
    rb | g
}

#[inline]
fn cov(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

pub struct Pix<'a> {
    pub p: &'a mut [u32],
    pub w: i32,
    pub h: i32,
}

impl<'a> Pix<'a> {
    pub fn new(p: &'a mut [u32], w: i32, h: i32) -> Self {
        debug_assert_eq!(p.len(), (w * h) as usize);
        Pix { p, w, h }
    }

    #[inline]
    fn put(&mut self, x: i32, y: i32, paint: Paint, coverage: f32) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return;
        }
        let a = (paint.a as f32 * coverage + 0.5) as u32;
        let i = (y * self.w + x) as usize;
        self.p[i] = blend(self.p[i], paint.rgb, a);
    }

    pub fn get(&self, x: i32, y: i32) -> Option<u32> {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return None;
        }
        Some(self.p[(y * self.w + x) as usize])
    }

    pub fn clear(&mut self, paint: Paint) {
        if paint.a >= 255 {
            self.p.fill(paint.rgb);
        } else {
            for d in self.p.iter_mut() {
                *d = blend(*d, paint.rgb, paint.a);
            }
        }
    }

    pub fn pixel(&mut self, x: i32, y: i32, paint: Paint) {
        self.put(x, y, paint, 1.0);
    }

    /// Retângulo preenchido (sem AA; coordenadas inteiras).
    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, paint: Paint) {
        let x0 = x.max(0);
        let y0 = y.max(0);
        let x1 = (x.saturating_add(w)).min(self.w);
        let y1 = (y.saturating_add(h)).min(self.h);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        for yy in y0..y1 {
            let row = &mut self.p[(yy * self.w + x0) as usize..(yy * self.w + x1) as usize];
            if paint.a >= 255 {
                row.fill(paint.rgb);
            } else {
                for d in row.iter_mut() {
                    *d = blend(*d, paint.rgb, paint.a);
                }
            }
        }
    }

    /// thick <= 0: preenchido; > 0: só a borda, com essa espessura (para dentro).
    pub fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, paint: Paint, thick: i32) {
        if thick <= 0 || thick * 2 >= w.min(h) {
            self.fill_rect(x, y, w, h, paint);
            return;
        }
        let t = thick;
        self.fill_rect(x, y, w, t, paint);
        self.fill_rect(x, y + h - t, w, t, paint);
        self.fill_rect(x, y + t, t, h - 2 * t, paint);
        self.fill_rect(x + w - t, y + t, t, h - 2 * t, paint);
    }

    /// Retângulo preenchido com cantos arredondados de raio r (AA).
    pub fn round_rect(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, paint: Paint) {
        if w <= 0 || h <= 0 {
            return;
        }
        let r = (r.max(0) as f32).min(w.min(h) as f32 / 2.0);
        let cx = x as f32 + w as f32 / 2.0;
        let cy = y as f32 + h as f32 / 2.0;
        let hx = w as f32 / 2.0 - r;
        let hy = h as f32 / 2.0 - r;
        for yy in y.max(0)..(y + h).min(self.h) {
            for xx in x.max(0)..(x + w).min(self.w) {
                let qx = (xx as f32 + 0.5 - cx).abs() - hx;
                let qy = (yy as f32 + 0.5 - cy).abs() - hy;
                let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
                let d = outside + qx.max(qy).min(0.0) - r;
                self.put(xx, yy, paint, cov(0.5 - d));
            }
        }
    }

    /// Círculo de centro (cx, cy) e raio r. thick <= 0: disco; > 0: anel
    /// com essa espessura (para dentro do raio).
    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, paint: Paint, thick: f32) {
        if r <= 0.0 {
            return;
        }
        let x0 = ((cx - r - 1.0).floor() as i32).max(0);
        let x1 = ((cx + r + 1.0).ceil() as i32).min(self.w - 1);
        let y0 = ((cy - r - 1.0).floor() as i32).max(0);
        let y1 = ((cy + r + 1.0).ceil() as i32).min(self.h - 1);
        let ring = thick > 0.0 && thick < r;
        let half = thick / 2.0;
        let mid = r - half;
        let inner2 = if r > 1.0 { (r - 1.0) * (r - 1.0) } else { 0.0 };
        for y in y0..=y1 {
            let dy = y as f32 + 0.5 - cy;
            for x in x0..=x1 {
                let dx = x as f32 + 0.5 - cx;
                let d2 = dx * dx + dy * dy;
                if !ring {
                    if d2 <= inner2 {
                        self.put(x, y, paint, 1.0);
                    } else {
                        self.put(x, y, paint, cov(r - d2.sqrt() + 0.5));
                    }
                } else {
                    let d = d2.sqrt();
                    self.put(x, y, paint, cov(half + 0.5 - (d - mid).abs()));
                }
            }
        }
    }

    /// Linha de (x0,y0) a (x1,y1) com pontas arredondadas e espessura `thick` (AA).
    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, paint: Paint, thick: f32) {
        let rad = (thick.max(1.0)) / 2.0;
        let (dx, dy) = (x1 - x0, y1 - y0);
        let len2 = dx * dx + dy * dy;
        let ya = ((y0.min(y1) - rad - 1.0).floor() as i32).max(0);
        let yb = ((y0.max(y1) + rad + 1.0).ceil() as i32).min(self.h - 1);
        let bx0 = ((x0.min(x1) - rad - 1.0).floor() as i32).max(0);
        let bx1 = ((x0.max(x1) + rad + 1.0).ceil() as i32).min(self.w - 1);
        let reach = rad + 1.5;
        for y in ya..=yb {
            let py = y as f32 + 0.5;
            // Intervalo de x que contém a cápsula nesta linha (superconjunto).
            let (mut lo, mut hi) = (f32::MAX, f32::MIN);
            for (ex, ey) in [(x0, y0), (x1, y1)] {
                let t = py - ey;
                if t.abs() <= reach {
                    let s = (reach * reach - t * t).max(0.0).sqrt();
                    lo = lo.min(ex - s);
                    hi = hi.max(ex + s);
                }
            }
            if dy.abs() > 1e-6 {
                let xc = x0 + (py - y0) * dx / dy;
                let half = reach * len2.sqrt() / dy.abs();
                lo = lo.min(xc - half);
                hi = hi.max(xc + half);
            } else {
                lo = bx0 as f32;
                hi = bx1 as f32;
            }
            if lo > hi {
                continue;
            }
            let xa = (lo.floor() as i32).max(bx0);
            let xb = (hi.ceil() as i32).min(bx1);
            for x in xa..=xb {
                let px = x as f32 + 0.5;
                let t = if len2 > 0.0 { (((px - x0) * dx + (py - y0) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
                let qx = px - (x0 + t * dx);
                let qy = py - (y0 + t * dy);
                let d = (qx * qx + qy * qy).sqrt();
                self.put(x, y, paint, cov(rad - d + 0.5));
            }
        }
    }

    /// Triângulo preenchido (AA).
    pub fn triangle(&mut self, p: [(f32, f32); 3], paint: Paint) {
        let area = (p[1].0 - p[0].0) * (p[2].1 - p[0].1) - (p[1].1 - p[0].1) * (p[2].0 - p[0].0);
        if area.abs() < 1e-6 {
            return;
        }
        let sign = area.signum();
        let edges: Vec<(f32, f32, f32, f32, f32)> = (0..3)
            .map(|i| {
                let (ax, ay) = p[i];
                let (bx, by) = p[(i + 1) % 3];
                let len = ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt();
                (ax, ay, bx - ax, by - ay, len)
            })
            .collect();
        let x0 = (p.iter().map(|q| q.0).fold(f32::MAX, f32::min).floor() as i32 - 1).max(0);
        let x1 = (p.iter().map(|q| q.0).fold(f32::MIN, f32::max).ceil() as i32 + 1).min(self.w - 1);
        let y0 = (p.iter().map(|q| q.1).fold(f32::MAX, f32::min).floor() as i32 - 1).max(0);
        let y1 = (p.iter().map(|q| q.1).fold(f32::MIN, f32::max).ceil() as i32 + 1).min(self.h - 1);
        for y in y0..=y1 {
            let py = y as f32 + 0.5;
            for x in x0..=x1 {
                let px = x as f32 + 0.5;
                let mut dmin = f32::MAX;
                for &(ax, ay, ex, ey, len) in &edges {
                    let d = sign * (ex * (py - ay) - ey * (px - ax)) / len;
                    dmin = dmin.min(d);
                }
                if dmin > -0.5 {
                    self.put(x, y, paint, cov(dmin + 0.5));
                }
            }
        }
    }

    /// Degradê de c1 para c2 (vertical: de cima para baixo; senão, da esquerda para a direita).
    pub fn gradient(&mut self, x: i32, y: i32, w: i32, h: i32, c1: Paint, c2: Paint, vertical: bool) {
        if w <= 0 || h <= 0 {
            return;
        }
        let steps = if vertical { h } else { w };
        for i in 0..steps {
            let t = if steps > 1 { i as f32 / (steps - 1) as f32 } else { 0.0 };
            let lerp = |a: u32, b: u32, sh: u32| -> u32 {
                let a = ((a >> sh) & 0xFF) as f32;
                let b = ((b >> sh) & 0xFF) as f32;
                ((a + (b - a) * t + 0.5) as u32) << sh
            };
            let rgb = lerp(c1.rgb, c2.rgb, 16) | lerp(c1.rgb, c2.rgb, 8) | lerp(c1.rgb, c2.rgb, 0);
            let a = (c1.a as f32 + (c2.a as f32 - c1.a as f32) * t + 0.5) as u32;
            let paint = Paint { rgb, a };
            if vertical {
                self.fill_rect(x, y + i, w, 1, paint);
            } else {
                self.fill_rect(x + i, y, 1, h, paint);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paint_decoding() {
        assert_eq!(Paint::from_i64(0xFF8800), Paint { rgb: 0xFF8800, a: 255 });
        assert_eq!(Paint::from_i64(0x1_80FF8800), Paint { rgb: 0xFF8800, a: 0x80 });
        assert_eq!(Paint::from_i64(0x1_00000000), Paint { rgb: 0, a: 0 });
    }

    #[test]
    fn blend_halves() {
        assert_eq!(blend(0x000000, 0xFFFFFF, 255), 0xFFFFFF);
        assert_eq!(blend(0x123456, 0xFFFFFF, 0), 0x123456);
        let m = blend(0x000000, 0xFF00FF, 128);
        assert!(((m >> 16) & 0xFF).abs_diff(128) <= 1 && (m & 0xFF).abs_diff(128) <= 1 && (m >> 8) & 0xFF == 0);
    }

    #[test]
    fn shapes_cover_their_area() {
        let mut buf = vec![0u32; 100 * 100];
        let mut p = Pix::new(&mut buf, 100, 100);
        let red = Paint::from_i64(0xFF0000);
        p.circle(50.0, 50.0, 20.0, red, 0.0);
        assert_eq!(p.get(50, 50), Some(0xFF0000));
        assert_eq!(p.get(50, 75), Some(0));
        p.clear(Paint::from_i64(0));
        p.line(10.0, 10.0, 90.0, 90.0, red, 3.0);
        assert_eq!(p.get(50, 50), Some(0xFF0000));
        assert_eq!(p.get(90, 10), Some(0));
        p.clear(Paint::from_i64(0));
        p.triangle([(10.0, 90.0), (50.0, 10.0), (90.0, 90.0)], red);
        assert_eq!(p.get(50, 60), Some(0xFF0000));
        assert_eq!(p.get(5, 5), Some(0));
        p.rect(0, 0, 100, 100, red, 2);
        assert_eq!(p.get(1, 50), Some(0xFF0000));
        assert_eq!(p.get(3, 50), Some(0));
        p.gradient(0, 0, 100, 100, Paint::from_i64(0), Paint::from_i64(0x0000FF), true);
        assert_eq!(p.get(0, 0), Some(0));
        assert_eq!(p.get(0, 99), Some(0xFF));
        p.round_rect(10, 10, 80, 80, 20, red);
        assert_eq!(p.get(50, 50), Some(0xFF0000));
        assert_ne!(p.get(11, 11), Some(0xFF0000));
        // fora da tela não quebra
        p.circle(-50.0, -50.0, 10.0, red, 0.0);
        p.line(-500.0, 50.0, 500.0, 50.0, red, 1.0);
        p.fill_rect(-10, -10, 5000, 5, red);
    }
}
