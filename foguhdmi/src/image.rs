//! Decodificador de BMP (24/32 bpp, BI_RGB ou BI_BITFIELDS, nas duas
//! orientações) e cópia de imagem para o canvas com escala e alfa.
//! É o formato que `img.encode("bmp")` do fogu:image produz.

use crate::pix::{Pix, blend};

pub struct Image {
    pub w: i32,
    pub h: i32,
    /// 0xAARRGGBB
    pub px: Vec<u32>,
}

/// Base64 padrão (aceita também um prefixo "data:...;base64," e quebras de linha).
pub fn decode_base64(s: &str) -> Result<Vec<u8>, String> {
    let s = match s.find(";base64,") {
        Some(i) => &s[i + 8..],
        None => s,
    };
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\r' | b'\n' | b' ' => continue,
            _ => return Err(format!("invalid base64 character {:?}", c as char)),
        } as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

fn u16_at(b: &[u8], o: usize) -> Result<u32, String> {
    b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]) as u32).ok_or_else(|| "bmp: truncated header".to_string())
}

fn u32_at(b: &[u8], o: usize) -> Result<u32, String> {
    b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]])).ok_or_else(|| "bmp: truncated header".to_string())
}

fn channel(v: u32, mask: u32) -> u32 {
    if mask == 0 {
        return 255;
    }
    let shift = mask.trailing_zeros();
    let bits = (mask >> shift).count_ones();
    let x = (v & mask) >> shift;
    if bits >= 8 { x >> (bits - 8) } else { (x * 255) / ((1 << bits) - 1) }
}

pub fn decode_bmp(b: &[u8]) -> Result<Image, String> {
    if b.len() < 54 || &b[0..2] != b"BM" {
        return Err("not a BMP image".into());
    }
    let offset = u32_at(b, 10)? as usize;
    let hsize = u32_at(b, 14)?;
    let w = u32_at(b, 18)? as i32;
    let raw_h = u32_at(b, 22)? as i32;
    let bpp = u16_at(b, 28)?;
    let comp = u32_at(b, 30)?;
    if w <= 0 || raw_h == 0 || w > 16384 || raw_h.abs() > 16384 {
        return Err(format!("bmp: invalid size {w}x{raw_h}"));
    }
    let (top_down, h) = (raw_h < 0, raw_h.abs());
    let (mut rm, mut gm, mut bm, mut am) = (0x00FF0000u32, 0x0000FF00u32, 0x000000FFu32, 0u32);
    match (bpp, comp) {
        (24, 0) => {}
        (32, 0) => {}
        (32, 3) | (32, 6) => {
            rm = u32_at(b, 54)?;
            gm = u32_at(b, 58)?;
            bm = u32_at(b, 62)?;
            am = if hsize >= 56 || comp == 6 { u32_at(b, 66)? } else { 0 };
        }
        _ => return Err(format!("bmp: unsupported format ({bpp} bpp, compression {comp})")),
    }
    let bytes_pp = (bpp / 8) as usize;
    let stride = (w as usize * bytes_pp + 3) / 4 * 4;
    if b.len() < offset + stride * h as usize {
        return Err("bmp: truncated pixel data".into());
    }
    let mut px = vec![0u32; (w * h) as usize];
    for row in 0..h as usize {
        let src = offset + stride * if top_down { row } else { h as usize - 1 - row };
        let dst = &mut px[row * w as usize..(row + 1) * w as usize];
        for (x, d) in dst.iter_mut().enumerate() {
            let s = src + x * bytes_pp;
            *d = if bpp == 24 {
                0xFF00_0000 | (b[s + 2] as u32) << 16 | (b[s + 1] as u32) << 8 | b[s] as u32
            } else {
                let v = u32::from_le_bytes([b[s], b[s + 1], b[s + 2], b[s + 3]]);
                let a = if am == 0 { 255 } else { channel(v, am) };
                a << 24 | channel(v, rm) << 16 | channel(v, gm) << 8 | channel(v, bm)
            };
        }
    }
    Ok(Image { w, h, px })
}

impl Pix<'_> {
    /// Desenha a imagem em (x, y) com tamanho w x h (<= 0: tamanho original),
    /// escala bilinear e transparência pelo canal alfa.
    pub fn image(&mut self, img: &Image, x: i32, y: i32, w: i32, h: i32) {
        let (w, h) = if w <= 0 || h <= 0 { (img.w, img.h) } else { (w, h) };
        let same = w == img.w && h == img.h;
        let sx = img.w as f32 / w as f32;
        let sy = img.h as f32 / h as f32;
        for dy in (0.max(-y))..h.min(self.h - y) {
            let ty = y + dy;
            for dx in (0.max(-x))..w.min(self.w - x) {
                let tx = x + dx;
                let c = if same {
                    img.px[(dy * img.w + dx) as usize]
                } else {
                    sample(img, (dx as f32 + 0.5) * sx - 0.5, (dy as f32 + 0.5) * sy - 0.5)
                };
                let a = c >> 24;
                let i = (ty * self.w + tx) as usize;
                self.p[i] = blend(self.p[i], c & 0xFF_FFFF, a);
            }
        }
    }
}

fn sample(img: &Image, fx: f32, fy: f32) -> u32 {
    let fx = fx.clamp(0.0, (img.w - 1) as f32);
    let fy = fy.clamp(0.0, (img.h - 1) as f32);
    let (x0, y0) = (fx as i32, fy as i32);
    let (x1, y1) = ((x0 + 1).min(img.w - 1), (y0 + 1).min(img.h - 1));
    let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
    let p = |x: i32, y: i32| img.px[(y * img.w + x) as usize];
    let (c00, c10, c01, c11) = (p(x0, y0), p(x1, y0), p(x0, y1), p(x1, y1));
    let mut out = 0u32;
    for sh in [0u32, 8, 16, 24] {
        let ch = |c: u32| ((c >> sh) & 0xFF) as f32;
        let top = ch(c00) + (ch(c10) - ch(c00)) * tx;
        let bot = ch(c01) + (ch(c11) - ch(c01)) * tx;
        out |= ((top + (bot - top) * ty + 0.5) as u32 & 0xFF) << sh;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode_bmp;

    #[test]
    fn decodes_fogu_image_bmp() {
        // 3x2 gerado por fogu:image: laranja com (0,0) azul a 50%.
        let hex = "424d92000000000000007a0000006c0000000300000002000000010020000300000018000000000000000000000000000000000000000000ff0000ff0000ff000000000000ff";
        let mut b: Vec<u8> = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect();
        b.resize(0x7a, 0);
        // linha de baixo (y=1): 3x laranja opaco; linha de cima (y=0): azul 50%, 2x laranja
        for _ in 0..3 {
            b.extend_from_slice(&[0x00, 0x88, 0xFF, 0xFF]);
        }
        b.extend_from_slice(&[0xFF, 0x00, 0x00, 0x80]);
        for _ in 0..2 {
            b.extend_from_slice(&[0x00, 0x88, 0xFF, 0xFF]);
        }
        let img = decode_bmp(&b).unwrap();
        assert_eq!((img.w, img.h), (3, 2));
        assert_eq!(img.px[0], 0x800000FF);
        assert_eq!(img.px[1], 0xFFFF8800);
        assert_eq!(img.px[5], 0xFFFF8800);
    }

    #[test]
    fn roundtrip_24bit_and_draw_scaled() {
        let src = [0xFF0000, 0x00FF00, 0x0000FF, 0xFFFFFF];
        let img = decode_bmp(&encode_bmp(&src, 2, 2)).unwrap();
        assert_eq!(img.px, vec![0xFFFF0000, 0xFF00FF00, 0xFF0000FF, 0xFFFFFFFF]);
        let mut buf = vec![0u32; 10 * 10];
        let mut p = Pix::new(&mut buf, 10, 10);
        p.image(&img, 1, 1, 0, 0);
        assert_eq!(p.get(1, 1), Some(0xFF0000));
        assert_eq!(p.get(2, 2), Some(0xFFFFFF));
        p.image(&img, 0, 0, 10, 10);
        assert_eq!(p.get(0, 0), Some(0xFF0000));
        assert_eq!(p.get(9, 9), Some(0xFFFFFF));
        p.image(&img, -5, -5, 0, 0); // fora da tela não quebra
        assert!(decode_bmp(b"nope").is_err());
        assert_eq!(decode_base64("Rm9ndQ==").unwrap(), b"Fogu");
        assert_eq!(decode_base64("data:image/bmp;base64,SGk=").unwrap(), b"Hi");
    }
}
