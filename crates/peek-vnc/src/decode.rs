//! Framebuffer encodings. We always request a 32-bit little-endian true-colour
//! format with red at bit 0, green at 8 and blue at 16, so a wire pixel is
//! already our in-memory RGBA layout once alpha is set, and the 3-byte
//! "compressed pixel" (ZRLE CPIXEL / Tight TPIXEL) is simply `[r, g, b]`.

use crate::io::Reader;
use anyhow::{Context, bail, ensure};
use flate2::{Decompress, FlushDecompress, Status};
use tokio::io::AsyncRead;

pub const ENC_RAW: i32 = 0;
pub const ENC_COPYRECT: i32 = 1;
pub const ENC_RRE: i32 = 2;
pub const ENC_HEXTILE: i32 = 5;
pub const ENC_TIGHT: i32 = 7;
pub const ENC_ZRLE: i32 = 16;
pub const ENC_PSEUDO_JPEG_QUALITY_0: i32 = -32;
pub const ENC_PSEUDO_COMPRESS_0: i32 = -256;
pub const ENC_PSEUDO_DESKTOP_SIZE: i32 = -223;
pub const ENC_PSEUDO_LAST_RECT: i32 = -224;
pub const ENC_PSEUDO_CURSOR: i32 = -239;
pub const ENC_PSEUDO_EXT_DESKTOP_SIZE: i32 = -308;

const OPAQUE: u32 = 0xFF00_0000;

#[inline(always)]
pub fn px4(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], 0xFF])
}

#[inline(always)]
pub fn px3(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], 0xFF])
}

/// Convert wire pixels (4 bytes each) to framebuffer pixels in place.
pub fn wire_to_pixels(bytes: &[u8], out: &mut Vec<u32>) {
    out.clear();
    out.extend(bytes.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c) | OPAQUE));
}

/// Bounds-checked cursor over a decompressed buffer.
struct Bytes<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Bytes<'a> {
    fn new(d: &'a [u8]) -> Self {
        Self { d, p: 0 }
    }

    #[inline]
    fn take(&mut self, n: usize) -> anyhow::Result<&'a [u8]> {
        let end = self.p.checked_add(n).filter(|&e| e <= self.d.len()).context("truncated data")?;
        let s = &self.d[self.p..end];
        self.p = end;
        Ok(s)
    }

    #[inline]
    fn u8(&mut self) -> anyhow::Result<u8> {
        Ok(self.take(1)?[0])
    }

    #[inline]
    fn cpixel(&mut self) -> anyhow::Result<u32> {
        Ok(px3(self.take(3)?))
    }
}

/// Inflate exactly `out_len` bytes (or, if `None`, everything available, up
/// to `max` bytes) from `input` with a persistent zlib stream.
pub fn inflate(
    z: &mut Decompress,
    input: &[u8],
    out_len: Option<usize>,
    max: usize,
    out: &mut Vec<u8>,
) -> anyhow::Result<()> {
    out.clear();
    let mut consumed = 0usize;
    match out_len {
        Some(n) => out.reserve_exact(n),
        None => out.reserve(input.len() * 4 + 1024),
    }
    loop {
        if out_len.is_some_and(|n| out.len() >= n) {
            break;
        }
        ensure!(out.len() <= max, "compressed data expands beyond {max} bytes");
        if out.len() == out.capacity() {
            out.reserve(out.len().max(4096).min(max + 1 - out.len()));
        }
        let (bi, bo) = (z.total_in(), z.total_out());
        let status = z.decompress_vec(&input[consumed..], out, FlushDecompress::Sync).context("zlib error")?;
        consumed += (z.total_in() - bi) as usize;
        let stalled = z.total_in() == bi && z.total_out() == bo;
        let output_room_left = out.len() < out.capacity();
        // Done when the stream ends, or all input is used and zlib stopped
        // for lack of input rather than lack of output space.
        if status == Status::StreamEnd || (consumed >= input.len() && output_room_left) || (stalled && output_room_left)
        {
            break;
        }
    }
    if let Some(n) = out_len {
        ensure!(out.len() == n, "zlib stream produced {} bytes, expected {n}", out.len());
    }
    Ok(())
}

/// Decode the inflated payload of a ZRLE rectangle into `out` (w*h pixels).
pub fn zrle(data: &[u8], w: u32, h: u32, out: &mut Vec<u32>) -> anyhow::Result<()> {
    let (w, h) = (w as usize, h as usize);
    out.clear();
    out.resize(w * h, OPAQUE);
    let mut b = Bytes::new(data);
    let mut palette = [0u32; 128];

    for ty in (0..h).step_by(64) {
        let th = 64.min(h - ty);
        for tx in (0..w).step_by(64) {
            let tw = 64.min(w - tx);
            let sub = b.u8()?;
            let mut put = |x: usize, y: usize, p: u32| out[(ty + y) * w + tx + x] = p;
            match sub {
                0 => {
                    for y in 0..th {
                        let row = b.take(tw * 3)?;
                        for x in 0..tw {
                            put(x, y, px3(&row[x * 3..]));
                        }
                    }
                }
                1 => {
                    let p = b.cpixel()?;
                    for y in 0..th {
                        out[(ty + y) * w + tx..(ty + y) * w + tx + tw].fill(p);
                    }
                }
                2..=16 => {
                    let n = sub as usize;
                    for c in palette.iter_mut().take(n) {
                        *c = b.cpixel()?;
                    }
                    let bits = match n {
                        2 => 1,
                        3..=4 => 2,
                        _ => 4,
                    };
                    let row_bytes = (tw * bits).div_ceil(8);
                    let mask = (1u8 << bits) - 1;
                    for y in 0..th {
                        let row = b.take(row_bytes)?;
                        for x in 0..tw {
                            let bit = x * bits;
                            let idx = (row[bit / 8] >> (8 - bits - bit % 8)) & mask;
                            put(x, y, palette[idx as usize]);
                        }
                    }
                }
                128 | 130..=255 => {
                    let n = if sub == 128 { 0 } else { (sub - 128) as usize };
                    for c in palette.iter_mut().take(n) {
                        *c = b.cpixel()?;
                    }
                    let total = tw * th;
                    let mut i = 0;
                    while i < total {
                        let (p, run) = if n == 0 {
                            (b.cpixel()?, rle_len(&mut b)?)
                        } else {
                            let idx = b.u8()?;
                            let p = *palette.get((idx & 0x7f) as usize).context("bad palette index")?;
                            if idx & 0x80 != 0 { (p, rle_len(&mut b)?) } else { (p, 1) }
                        };
                        ensure!(i + run <= total, "ZRLE run overflows tile");
                        for k in i..i + run {
                            put(k % tw, k / tw, p);
                        }
                        i += run;
                    }
                }
                _ => bail!("invalid ZRLE subencoding {sub}"),
            }
        }
    }
    Ok(())
}

fn rle_len(b: &mut Bytes<'_>) -> anyhow::Result<usize> {
    let mut len = 1usize;
    loop {
        let v = b.u8()?;
        len += v as usize;
        if v != 255 {
            return Ok(len);
        }
    }
}

/// Hextile reads tile by tile from the stream; there is no length prefix.
pub async fn hextile<R: AsyncRead + Unpin>(
    r: &mut Reader<R>,
    w: u32,
    h: u32,
    out: &mut Vec<u32>,
) -> anyhow::Result<()> {
    const RAW: u8 = 1;
    const BG: u8 = 2;
    const FG: u8 = 4;
    const ANY_SUBRECTS: u8 = 8;
    const COLOURED: u8 = 16;

    let (w, h) = (w as usize, h as usize);
    out.clear();
    out.resize(w * h, OPAQUE);
    let (mut bg, mut fg) = (OPAQUE, OPAQUE);
    let mut buf = Vec::with_capacity(16 * 16 * 4);
    let mut px = [0u8; 4];

    for ty in (0..h).step_by(16) {
        let th = 16.min(h - ty);
        for tx in (0..w).step_by(16) {
            let tw = 16.min(w - tx);
            let mask = r.u8().await?;
            if mask & RAW != 0 {
                r.read_into(tw * th * 4, &mut buf).await?;
                for y in 0..th {
                    for x in 0..tw {
                        out[(ty + y) * w + tx + x] = px4(&buf[(y * tw + x) * 4..]);
                    }
                }
                continue;
            }
            if mask & BG != 0 {
                r.exact(&mut px).await?;
                bg = px4(&px);
            }
            if mask & FG != 0 {
                r.exact(&mut px).await?;
                fg = px4(&px);
            }
            for y in 0..th {
                out[(ty + y) * w + tx..(ty + y) * w + tx + tw].fill(bg);
            }
            if mask & ANY_SUBRECTS == 0 {
                continue;
            }
            let n = r.u8().await? as usize;
            let per = if mask & COLOURED != 0 { 6 } else { 2 };
            r.read_into(n * per, &mut buf).await?;
            for s in buf.chunks_exact(per) {
                let (color, xy, wh) = if per == 6 { (px4(s), s[4], s[5]) } else { (fg, s[0], s[1]) };
                let (sx, sy) = ((xy >> 4) as usize, (xy & 15) as usize);
                let (sw, sh) = ((wh >> 4) as usize + 1, (wh & 15) as usize + 1);
                for y in sy..(sy + sh).min(th) {
                    let row = (ty + y) * w + tx;
                    out[row + sx.min(tw)..row + (sx + sw).min(tw)].fill(color);
                }
            }
        }
    }
    Ok(())
}

pub async fn rre<R: AsyncRead + Unpin>(r: &mut Reader<R>, w: u32, h: u32, out: &mut Vec<u32>) -> anyhow::Result<()> {
    let (w, h) = (w as usize, h as usize);
    let n = r.u32().await? as usize;
    // Every subrectangle covers at least one pixel.
    ensure!(n <= w * h, "RRE claims {n} subrectangles for {w}x{h}");
    let mut px = [0u8; 4];
    r.exact(&mut px).await?;
    out.clear();
    out.resize(w * h, px4(&px));
    let mut buf = Vec::new();
    r.read_into(n.checked_mul(12).context("RRE too large")?, &mut buf).await?;
    for s in buf.as_chunks::<12>().0 {
        let color = px4(s);
        let be = |i: usize| u16::from_be_bytes([s[i], s[i + 1]]) as usize;
        let (sx, sy, sw, sh) = (be(4), be(6), be(8), be(10));
        for y in sy.min(h)..(sy + sh).min(h) {
            out[y * w + sx.min(w)..y * w + (sx + sw).min(w)].fill(color);
        }
    }
    Ok(())
}

/// Rich cursor pseudo-encoding: pixels followed by a 1-bit transparency mask.
pub fn cursor_rgba(pixels: &[u8], mask: &[u8], w: u32, h: u32) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let stride = w.div_ceil(8);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let p = &pixels[(y * w + x) * 4..];
            let visible = mask[y * stride + x / 8] & (0x80 >> (x % 8)) != 0;
            rgba.extend_from_slice(&[p[0], p[1], p[2], if visible { 255 } else { 0 }]);
        }
    }
    rgba
}

/// Tight encoding state: four persistent zlib streams.
pub struct Tight {
    zs: [Decompress; 4],
    zbuf: Vec<u8>,
    raw: Vec<u8>,
}

impl Default for Tight {
    fn default() -> Self {
        Self { zs: std::array::from_fn(|_| Decompress::new(true)), zbuf: Vec::new(), raw: Vec::new() }
    }
}

impl Tight {
    pub async fn decode<R: AsyncRead + Unpin>(
        &mut self,
        r: &mut Reader<R>,
        w: u32,
        h: u32,
        out: &mut Vec<u32>,
    ) -> anyhow::Result<()> {
        let (wu, hu) = (w as usize, h as usize);
        let ctl = r.u8().await?;
        for i in 0..4 {
            if ctl & (1 << i) != 0 {
                self.zs[i].reset(true);
            }
        }
        out.clear();
        match ctl >> 4 {
            0x8 => {
                let mut p = [0u8; 3];
                r.exact(&mut p).await?;
                out.resize(wu * hu, px3(&p));
                return Ok(());
            }
            0x9 => {
                let len = r.compact_len().await?;
                r.read_into(len, &mut self.zbuf).await?;
                return decode_jpeg(&self.zbuf, w, h, out);
            }
            t if t & 0x8 != 0 => bail!("unsupported Tight compression type {t:#x}"),
            _ => {}
        }

        let stream = ((ctl >> 4) & 3) as usize;
        let filter = if ctl & 0x40 != 0 { r.u8().await? } else { 0 };
        let mut palette = [0u32; 256];
        let mut ncolors = 0usize;
        let raw_len = match filter {
            0 | 2 => wu * hu * 3,
            1 => {
                ncolors = r.u8().await? as usize + 1;
                let mut p = vec![0u8; ncolors * 3];
                r.exact(&mut p).await?;
                for (i, c) in p.as_chunks::<3>().0.iter().enumerate() {
                    palette[i] = px3(c);
                }
                if ncolors == 2 { wu.div_ceil(8) * hu } else { wu * hu }
            }
            f => bail!("unknown Tight filter {f}"),
        };

        if raw_len < 12 {
            r.read_into(raw_len, &mut self.raw).await?;
        } else {
            let len = r.compact_len().await?;
            r.read_into(len, &mut self.zbuf).await?;
            inflate(&mut self.zs[stream], &self.zbuf, Some(raw_len), raw_len, &mut self.raw)?;
        }

        let data = &self.raw;
        out.reserve(wu * hu);
        match filter {
            0 => out.extend(data.as_chunks::<3>().0.iter().map(|c| px3(c))),
            1 if ncolors == 2 => {
                let stride = wu.div_ceil(8);
                for y in 0..hu {
                    for x in 0..wu {
                        let bit = data[y * stride + x / 8] >> (7 - x % 8) & 1;
                        out.push(palette[bit as usize]);
                    }
                }
            }
            1 => out.extend(data.iter().map(|&i| palette[i as usize])),
            2 => gradient(data, wu, hu, out),
            _ => unreachable!(),
        }
        Ok(())
    }
}

fn gradient(data: &[u8], w: usize, h: usize, out: &mut Vec<u32>) {
    let mut prev = vec![0u8; w * 3];
    let mut cur = vec![0u8; w * 3];
    for y in 0..h {
        for x in 0..w {
            for c in 0..3 {
                let left = if x > 0 { cur[(x - 1) * 3 + c] as i32 } else { 0 };
                let up = prev[x * 3 + c] as i32;
                let ul = if x > 0 { prev[(x - 1) * 3 + c] as i32 } else { 0 };
                let pred = (left + up - ul).clamp(0, 255);
                cur[x * 3 + c] = (pred as u8).wrapping_add(data[(y * w + x) * 3 + c]);
            }
        }
        out.extend(cur.as_chunks::<3>().0.iter().map(|c| px3(c)));
        std::mem::swap(&mut prev, &mut cur);
    }
}

fn decode_jpeg(data: &[u8], w: u32, h: u32, out: &mut Vec<u32>) -> anyhow::Result<()> {
    use zune_jpeg::JpegDecoder;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;

    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut dec = JpegDecoder::new_with_options(std::io::Cursor::new(data), opts);
    let rgba = dec.decode().map_err(|e| anyhow::anyhow!("JPEG: {e:?}"))?;
    let info = dec.info().context("JPEG without header")?;
    ensure!(info.width as u32 == w && info.height as u32 == h, "JPEG size mismatch");
    out.clear();
    out.extend(rgba.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes([c[0], c[1], c[2], 0xFF])));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use peek_core::rgb;

    #[test]
    fn zrle_solid_and_raw_tiles() {
        // 65x1: first tile 64 wide solid red, second tile 1 wide raw blue.
        let mut d = vec![1, 255, 0, 0];
        d.extend_from_slice(&[0, 0, 0, 255]);
        let mut out = vec![];
        zrle(&d, 65, 1, &mut out).unwrap();
        assert_eq!(out[0], rgb(255, 0, 0));
        assert_eq!(out[63], rgb(255, 0, 0));
        assert_eq!(out[64], rgb(0, 0, 255));
    }

    #[test]
    fn zrle_packed_palette() {
        // 4x1 tile, palette of 2, indices 0,1,1,0 -> bits 0110 0000
        let d = [2, 1, 1, 1, 9, 9, 9, 0b0110_0000];
        let mut out = vec![];
        zrle(&d, 4, 1, &mut out).unwrap();
        assert_eq!(out, vec![rgb(1, 1, 1), rgb(9, 9, 9), rgb(9, 9, 9), rgb(1, 1, 1)]);
    }

    #[test]
    fn zrle_palette_rle() {
        // 3x1, palette 2 (sub 130): idx1 run of 2 (0x81, 0), idx0 single
        let d = [130, 1, 1, 1, 9, 9, 9, 0x81, 1, 0x00];
        let mut out = vec![];
        zrle(&d, 3, 1, &mut out).unwrap();
        assert_eq!(out, vec![rgb(9, 9, 9), rgb(9, 9, 9), rgb(1, 1, 1)]);
    }

    #[test]
    fn zrle_rejects_truncated() {
        let mut out = vec![];
        assert!(zrle(&[0, 1, 2], 4, 4, &mut out).is_err());
        assert!(zrle(&[128, 1, 2, 3, 200], 2, 2, &mut out).is_err());
    }

    #[test]
    fn inflate_roundtrip_sync_flush() {
        use flate2::{Compress, Compression, FlushCompress};
        let mut c = Compress::new(Compression::default(), true);
        let mut z = Decompress::new(true);
        for msg in [&b"hello hello hello"[..], &b"second message on same stream"[..]] {
            let mut comp = Vec::with_capacity(256);
            c.compress_vec(msg, &mut comp, FlushCompress::Sync).unwrap();
            let mut out = vec![];
            inflate(&mut z, &comp, Some(msg.len()), msg.len(), &mut out).unwrap();
            assert_eq!(out, msg);
        }
    }

    #[test]
    fn gradient_filter() {
        // Zero residuals on 2x2 -> all black.
        let mut out = vec![];
        gradient(&[0; 12], 2, 2, &mut out);
        assert!(out.iter().all(|&p| p == rgb(0, 0, 0)));
        // Single pixel residual = value.
        out.clear();
        gradient(&[10, 20, 30], 1, 1, &mut out);
        assert_eq!(out[0], rgb(10, 20, 30));
    }

    #[test]
    fn cursor_mask() {
        let px = [1, 2, 3, 0, 4, 5, 6, 0];
        let rgba = cursor_rgba(&px, &[0b1000_0000], 2, 1);
        assert_eq!(rgba, vec![1, 2, 3, 255, 4, 5, 6, 0]);
    }

    /// Tiny deterministic PRNG so the test needs no extra crates.
    fn rng(seed: &mut u64) -> u64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed
    }

    /// Random input must produce errors, never panics or runaway allocations.
    #[test]
    fn decoders_survive_garbage() {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut out = Vec::new();
        for i in 0..3000 {
            let len = (rng(&mut seed) % 600) as usize;
            let mut data: Vec<u8> = (0..len).map(|_| rng(&mut seed) as u8).collect();
            // Bias some inputs toward valid ZRLE/Tight headers.
            if i % 3 == 0 && !data.is_empty() {
                data[0] = [0u8, 1, 2, 5, 16, 128, 130, 200, 0x80, 0x90, 0x40, 0x50][i % 12];
            }
            let w = (rng(&mut seed) % 80 + 1) as u32;
            let h = (rng(&mut seed) % 80 + 1) as u32;
            let _ = zrle(&data, w, h, &mut out);
            rt.block_on(async {
                let mut r = Reader::new(&data[..], None);
                let _ = hextile(&mut r, w, h, &mut out).await;
                let mut r = Reader::new(&data[..], None);
                let _ = rre(&mut r, w, h, &mut out).await;
                let mut r = Reader::new(&data[..], None);
                let _ = Tight::default().decode(&mut r, w, h, &mut out).await;
            });
        }
    }
}
