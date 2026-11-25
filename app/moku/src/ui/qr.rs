//! A QR code encoder and the generic `qr(text)` widget.
//!
//! The encoder is deliberately small: **byte mode, error correction M, versions 1-10** (up to
//! 213 bytes), which covers URLs, Wi-Fi strings and short text. It follows ISO/IEC 18004 and is
//! tested against matrices produced by an independent implementation (`qr_vectors.txt`, see the
//! tests), plus the format / version information words and the Reed-Solomon codewords of the
//! well-known "HELLO WORLD" example, and a decoder written in the tests from the standard that reads
//! every version and mask back to the input.
//!
//! * [`QrCode::encode`] — bytes → module matrix (`None` when it does not fit version 10).
//! * [`QrCode::render`] — matrix → premultiplied BGRA pixels (black on white, quiet zone).
//! * [`qr_image`] — text → a bitmap with a chosen module size, for a caller that lays the
//!   picture out itself (the phone-transfer card).
//! * [`qr`] — the widget: `qr("https://example.com")` is an image node sized to fit about 176
//!   DIP. The rendered bitmap is cached by text (the last two), so calling it from a builder on
//!   every flush costs a string comparison and never forces a redraw. Call [`clear_cache`] when
//!   the code is no longer shown.

use std::cell::RefCell;

use super::{Image, Node, caption, color, image};
use crate::i18n::tr;

/// Longest payload that fits version 10 at error correction M (byte mode).
pub const MAX_BYTES: usize = 213;
/// Quiet zone around the code, in modules (the specification asks for 4).
pub const QUIET: usize = 4;

/// Per version 1-10 at level M: EC codewords per block, then (blocks, data codewords per block)
/// for the first and the second group of blocks.
const SPECS: [(usize, usize, usize, usize, usize); 10] = [
    (10, 1, 16, 0, 0),
    (16, 1, 28, 0, 0),
    (26, 1, 44, 0, 0),
    (18, 2, 32, 0, 0),
    (24, 2, 43, 0, 0),
    (16, 4, 27, 0, 0),
    (18, 4, 31, 0, 0),
    (22, 2, 38, 2, 39),
    (22, 3, 36, 2, 37),
    (26, 4, 43, 1, 44),
];

/// Alignment pattern centre coordinates per version 1-10.
const ALIGN: [&[usize]; 10] = [&[], &[6, 18], &[6, 22], &[6, 26], &[6, 30], &[6, 34], &[6, 22, 38], &[6, 24, 42], &[6, 26, 46], &[6, 28, 50]];

/// A finished QR symbol.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct QrCode {
    version: u8,
    size: usize,
    /// Row-major, `true` = dark.
    modules: Vec<bool>,
    mask: u8,
}

impl QrCode {
    /// Encode `data` in byte mode at level M with the smallest version that fits. `None` when
    /// it is longer than [`MAX_BYTES`].
    pub fn encode(data: &[u8]) -> Option<QrCode> {
        QrCode::encode_with(data, None)
    }

    /// [`QrCode::encode`], optionally with a fixed mask pattern 0-7 (tests; the mask only changes
    /// how the modules look, never what a scanner reads).
    pub fn encode_with(data: &[u8], forced: Option<u8>) -> Option<QrCode> {
        let version = (1..=10u8).find(|&v| capacity(v) >= data.len())?;
        let codewords = interleave(version, &data_codewords(version, data));
        let size = 17 + 4 * version as usize;
        let mut b = Builder::new(version, size);
        b.draw_function_patterns();
        b.draw_codewords(&codewords);
        // The mask with the lowest penalty (the first one on a tie).
        let mut best: Option<(u32, u8, Vec<bool>)> = None;
        for mask in (0..8u8).filter(|m| forced.is_none_or(|f| f == *m)) {
            let mut m = b.clone();
            m.apply_mask(mask);
            m.draw_format(mask);
            let p = penalty(&m.modules, size);
            if best.as_ref().is_none_or(|(bp, _, _)| p < *bp) {
                best = Some((p, mask, m.modules));
            }
        }
        let (_, mask, modules) = best?;
        Some(QrCode { version, size, modules, mask })
    }

    pub fn encode_str(s: &str) -> Option<QrCode> {
        QrCode::encode(s.as_bytes())
    }

    pub fn version(&self) -> u8 {
        self.version
    }

    /// Modules per side (without the quiet zone).
    pub fn size(&self) -> usize {
        self.size
    }

    pub fn mask(&self) -> u8 {
        self.mask
    }

    /// `true` = dark. Out of range is light (the quiet zone).
    pub fn get(&self, x: usize, y: usize) -> bool {
        x < self.size && y < self.size && self.modules[y * self.size + x]
    }

    /// Black modules on white with a [`QUIET`] module border, every module `px` pixels square:
    /// `(side in pixels, BGRA)`. Opaque, so premultiplied or not makes no difference.
    pub fn render(&self, px: usize) -> (usize, Vec<u8>) {
        let px = px.max(1);
        let side = (self.size + 2 * QUIET) * px;
        let mut out = vec![255u8; side * side * 4];
        for y in 0..self.size {
            for x in 0..self.size {
                if !self.get(x, y) {
                    continue;
                }
                for dy in 0..px {
                    let row = ((y + QUIET) * px + dy) * side;
                    for dx in 0..px {
                        let i = (row + (x + QUIET) * px + dx) * 4;
                        out[i] = 0;
                        out[i + 1] = 0;
                        out[i + 2] = 0;
                    }
                }
            }
        }
        (side, out)
    }

    /// One hex string per row (4 modules per digit, the last digit padded with zeros), the form
    /// the test vectors use.
    #[cfg(test)]
    fn hex_rows(&self) -> Vec<String> {
        (0..self.size)
            .map(|y| {
                let mut s = String::new();
                for x0 in (0..self.size).step_by(4) {
                    let mut d = 0u32;
                    for k in 0..4 {
                        d = (d << 1) | self.get(x0 + k, y) as u32;
                    }
                    s.push(char::from_digit(d, 16).unwrap_or('0'));
                }
                s
            })
            .collect()
    }
}

/// How many bytes version `v` (1-10) holds at level M.
pub fn capacity(v: u8) -> usize {
    let (ec, na, da, nb, db) = SPECS[v as usize - 1];
    let _ = ec;
    let data_bits = (na * da + nb * db) * 8;
    let header = 4 + if v >= 10 { 16 } else { 8 };
    (data_bits - header) / 8
}

// ---- data codewords and error correction ---------------------------------------------------------

fn push_bits(bits: &mut Vec<bool>, value: u32, n: usize) {
    for i in (0..n).rev() {
        bits.push((value >> i) & 1 == 1);
    }
}

/// Mode indicator, character count, the bytes, terminator and pad bytes: exactly the data
/// codewords of `version`.
fn data_codewords(version: u8, data: &[u8]) -> Vec<u8> {
    let (_, na, da, nb, db) = SPECS[version as usize - 1];
    let total = na * da + nb * db;
    let mut bits: Vec<bool> = Vec::with_capacity(total * 8);
    push_bits(&mut bits, 0b0100, 4);
    push_bits(&mut bits, data.len() as u32, if version >= 10 { 16 } else { 8 });
    for &b in data {
        push_bits(&mut bits, b as u32, 8);
    }
    let room = total * 8 - bits.len();
    bits.extend(std::iter::repeat_n(false, room.min(4)));
    while bits.len() % 8 != 0 {
        bits.push(false);
    }
    let mut out: Vec<u8> = bits.chunks(8).map(|c| c.iter().fold(0u8, |a, &b| (a << 1) | b as u8)).collect();
    let mut pad = [0xECu8, 0x11].into_iter().cycle();
    while out.len() < total {
        out.extend(pad.next());
    }
    out
}

/// GF(256) with the QR polynomial 0x11D: `(exp, log)` tables.
fn gf_tables() -> ([u8; 512], [u8; 256]) {
    let mut exp = [0u8; 512];
    let mut log = [0u8; 256];
    let mut x = 1u32;
    for i in 0..255 {
        exp[i] = x as u8;
        log[x as usize] = i as u8;
        x <<= 1;
        if x & 0x100 != 0 {
            x ^= 0x11D;
        }
    }
    for i in 255..512 {
        exp[i] = exp[i - 255];
    }
    (exp, log)
}

/// The `ec` Reed-Solomon codewords of `data`.
fn reed_solomon(data: &[u8], ec: usize) -> Vec<u8> {
    let (exp, log) = gf_tables();
    let mul = |a: u8, b: u8| if a == 0 || b == 0 { 0 } else { exp[log[a as usize] as usize + log[b as usize] as usize] };
    // Generator polynomial: (x - a^0)(x - a^1)...(x - a^(ec-1)), highest power first, monic.
    let mut genpoly = vec![1u8];
    for i in 0..ec {
        let mut next = vec![0u8; genpoly.len() + 1];
        for (j, &c) in genpoly.iter().enumerate() {
            next[j] ^= c;
            next[j + 1] ^= mul(c, exp[i]);
        }
        genpoly = next;
    }
    // Long division: the remainder after the leading term is dropped.
    let mut rem = vec![0u8; ec];
    for &d in data {
        let factor = d ^ rem[0];
        rem.rotate_left(1);
        rem[ec - 1] = 0;
        for (r, &g) in rem.iter_mut().zip(&genpoly[1..]) {
            *r ^= mul(g, factor);
        }
    }
    rem
}

/// Split into blocks, add the EC codewords and interleave (data columns first, then EC).
fn interleave(version: u8, data: &[u8]) -> Vec<u8> {
    let (ec, na, da, nb, db) = SPECS[version as usize - 1];
    let mut blocks: Vec<(&[u8], Vec<u8>)> = vec![];
    let mut at = 0;
    for len in std::iter::repeat_n(da, na).chain(std::iter::repeat_n(db, nb)) {
        let d = &data[at..at + len];
        at += len;
        blocks.push((d, reed_solomon(d, ec)));
    }
    let longest = da.max(db);
    let mut out = Vec::with_capacity(data.len() + ec * (na + nb));
    for i in 0..longest {
        for (d, _) in &blocks {
            if let Some(&b) = d.get(i) {
                out.push(b);
            }
        }
    }
    for i in 0..ec {
        for (_, e) in &blocks {
            out.push(e[i]);
        }
    }
    out
}

// ---- the matrix -----------------------------------------------------------------------------------

#[derive(Clone)]
struct Builder {
    version: u8,
    size: usize,
    modules: Vec<bool>,
    func: Vec<bool>,
}

impl Builder {
    fn new(version: u8, size: usize) -> Builder {
        Builder { version, size, modules: vec![false; size * size], func: vec![false; size * size] }
    }

    fn set_func(&mut self, x: usize, y: usize, dark: bool) {
        self.modules[y * self.size + x] = dark;
        self.func[y * self.size + x] = true;
    }

    fn draw_function_patterns(&mut self) {
        let n = self.size;
        for i in 0..n {
            self.set_func(6, i, i % 2 == 0);
            self.set_func(i, 6, i % 2 == 0);
        }
        // Finder patterns with their separators.
        for (cx, cy) in [(3usize, 3usize), (n - 4, 3), (3, n - 4)] {
            for dy in -4i32..=4 {
                for dx in -4i32..=4 {
                    let (x, y) = (cx as i32 + dx, cy as i32 + dy);
                    if x < 0 || y < 0 || x >= n as i32 || y >= n as i32 {
                        continue;
                    }
                    let d = dx.abs().max(dy.abs());
                    self.set_func(x as usize, y as usize, d != 2 && d != 4);
                }
            }
        }
        // Alignment patterns, except the three that would overlap a finder.
        let pos = ALIGN[self.version as usize - 1];
        let last = pos.len().saturating_sub(1);
        for (i, &cy) in pos.iter().enumerate() {
            for (j, &cx) in pos.iter().enumerate() {
                if (i == 0 && j == 0) || (i == 0 && j == last) || (i == last && j == 0) {
                    continue;
                }
                for dy in -2i32..=2 {
                    for dx in -2i32..=2 {
                        let d = dx.abs().max(dy.abs());
                        self.set_func((cx as i32 + dx) as usize, (cy as i32 + dy) as usize, d != 1);
                    }
                }
            }
        }
        // Reserve the format areas (the real bits come with the mask) and the version blocks.
        self.draw_format(0);
        self.draw_version();
    }

    fn draw_format(&mut self, mask: u8) {
        let bits = format_bits(mask);
        let n = self.size;
        let bit = |i: usize| (bits >> i) & 1 == 1;
        for i in 0..=5 {
            self.set_func(8, i, bit(i));
        }
        self.set_func(8, 7, bit(6));
        self.set_func(8, 8, bit(7));
        self.set_func(7, 8, bit(8));
        for i in 9..15 {
            self.set_func(14 - i, 8, bit(i));
        }
        for i in 0..8 {
            self.set_func(n - 1 - i, 8, bit(i));
        }
        for i in 8..15 {
            self.set_func(8, n - 15 + i, bit(i));
        }
        self.set_func(8, n - 8, true); // the always-dark module
    }

    fn draw_version(&mut self) {
        let Some(bits) = version_bits(self.version) else { return };
        let n = self.size;
        for i in 0..18 {
            let dark = (bits >> i) & 1 == 1;
            let (a, b) = (n - 11 + i % 3, i / 3);
            self.set_func(a, b, dark);
            self.set_func(b, a, dark);
        }
    }

    /// Zigzag placement from the bottom-right corner, two columns at a time, skipping the
    /// vertical timing column.
    fn draw_codewords(&mut self, data: &[u8]) {
        let n = self.size;
        let mut i = 0usize;
        let mut right = n as i32 - 1;
        while right >= 1 {
            if right == 6 {
                right = 5;
            }
            for vert in 0..n {
                for j in 0..2 {
                    let x = (right - j) as usize;
                    let upward = ((right + 1) & 2) == 0;
                    let y = if upward { n - 1 - vert } else { vert };
                    if !self.func[y * n + x] && i < data.len() * 8 {
                        self.modules[y * n + x] = (data[i >> 3] >> (7 - (i & 7))) & 1 == 1;
                        i += 1;
                    }
                }
            }
            right -= 2;
        }
    }

    fn apply_mask(&mut self, mask: u8) {
        let n = self.size;
        for y in 0..n {
            for x in 0..n {
                if self.func[y * n + x] {
                    continue;
                }
                let invert = match mask {
                    0 => (x + y) % 2 == 0,
                    1 => y % 2 == 0,
                    2 => x % 3 == 0,
                    3 => (x + y) % 3 == 0,
                    4 => (x / 3 + y / 2) % 2 == 0,
                    5 => x * y % 2 + x * y % 3 == 0,
                    6 => (x * y % 2 + x * y % 3) % 2 == 0,
                    _ => ((x + y) % 2 + x * y % 3) % 2 == 0,
                };
                self.modules[y * n + x] ^= invert;
            }
        }
    }
}

/// The 15 format bits for level M (`00`) and `mask`: BCH(15,5) with 0x537, masked with 0x5412.
pub fn format_bits(mask: u8) -> u32 {
    let data = (mask & 7) as u32; // level M = 0b00 in the top two bits
    let mut rem = data;
    for _ in 0..10 {
        rem = (rem << 1) ^ ((rem >> 9) * 0x537);
    }
    ((data << 10) | rem) ^ 0x5412
}

/// The 18 version-information bits (versions 7+): BCH(18,6) with 0x1F25.
pub fn version_bits(version: u8) -> Option<u32> {
    if version < 7 {
        return None;
    }
    let mut rem = version as u32;
    for _ in 0..12 {
        rem = (rem << 1) ^ ((rem >> 11) * 0x1F25);
    }
    Some(((version as u32) << 12) | rem)
}

// ---- mask selection -------------------------------------------------------------------------------

/// The four penalty rules of the specification.
fn penalty(m: &[bool], n: usize) -> u32 {
    let at = |x: usize, y: usize| m[y * n + x];
    let mut score = 0u32;
    // N1: runs of five or more equal modules, along rows and columns.
    for horizontal in [true, false] {
        for a in 0..n {
            let mut run = 1u32;
            for b in 1..n {
                let (cur, prev) = if horizontal { (at(b, a), at(b - 1, a)) } else { (at(a, b), at(a, b - 1)) };
                if cur == prev {
                    run += 1;
                    if run == 5 {
                        score += 3;
                    } else if run > 5 {
                        score += 1;
                    }
                } else {
                    run = 1;
                }
            }
        }
    }
    // N2: 2 x 2 blocks of one colour.
    for y in 0..n - 1 {
        for x in 0..n - 1 {
            let c = at(x, y);
            if c == at(x + 1, y) && c == at(x, y + 1) && c == at(x + 1, y + 1) {
                score += 3;
            }
        }
    }
    // N3: dark-light-dark-dark-dark-light-dark with four light modules on one side. Beyond the
    // symbol is the quiet zone, which is light; an occurrence costs 40 once, whichever side is
    // light (the same reading the reference encoder in the tests uses).
    const PAT: [bool; 7] = [true, false, true, true, true, false, true];
    for horizontal in [true, false] {
        for a in 0..n {
            let line = |i: isize| i >= 0 && (i as usize) < n && if horizontal { at(i as usize, a) } else { at(a, i as usize) };
            for s in 0..=n.saturating_sub(7) as isize {
                if n < 7 || !(0..7).all(|k| line(s + k) == PAT[k as usize]) {
                    continue;
                }
                let light_before = (s - 4..s).all(|i| !line(i));
                let light_after = (s + 7..s + 11).all(|i| !line(i));
                if light_before || light_after {
                    score += 40;
                }
            }
        }
    }
    // N4: how far the share of dark modules is from 50 %, in steps of 5 %.
    let dark = m.iter().filter(|&&d| d).count();
    let ratio = dark * 100 / (n * n);
    score += (ratio.abs_diff(50) / 5) as u32 * 10;
    score
}

// ---- the widget -----------------------------------------------------------------------------------

/// Pixels per module of the cached bitmap (the scene shrinks it to the node's size).
const BITMAP_PX: usize = 8;
/// The widget's target side in DIP; the module size is the largest whole DIP that fits.
const TARGET_DIP: f32 = 176.0;

thread_local! {
    /// The last two codes: `(text, modules per side, bitmap)`.
    static CACHE: RefCell<Vec<(String, usize, Image)>> = const { RefCell::new(Vec::new()) };
}

/// Drop the cached bitmaps (and the text they were made from).
pub fn clear_cache() {
    CACHE.with(|c| c.borrow_mut().clear());
}

/// The bitmap and module count for `text`, from the cache when possible.
fn cached(text: &str) -> Option<(usize, Image)> {
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if let Some((_, n, img)) = c.iter().find(|(t, _, _)| t == text) {
            return Some((*n, img.clone()));
        }
        let code = QrCode::encode_str(text)?;
        let (side, bgra) = code.render(BITMAP_PX);
        let img = Image::new(side as u32, side as u32, bgra);
        c.insert(0, (text.to_string(), code.size(), img.clone()));
        c.truncate(2);
        Some((code.size(), img))
    })
}

/// A QR code for `text`: a white image node of about 176 DIP with rounded corners. Text that
/// does not fit (over [`MAX_BYTES`]) gives a short caption instead.
pub fn qr(text: &str) -> Node {
    match cached(text) {
        Some((modules, img)) => {
            let total = (modules + 2 * QUIET) as f32;
            let side = (TARGET_DIP / total).floor().max(2.0) * total;
            image(&img, 10.0).size(side, side)
        }
        None => caption(tr("Teks terlalu panjang untuk kode QR", "Text is too long for a QR code")).color(color::TEXT3),
    }
}

/// The bitmap of a code for `text` with `module_px` pixels per module and the [`QUIET`] zone
/// around it (black on white, opaque), for a caller that lays the picture out itself: the
/// phone-transfer card. `None` when the text is longer than [`MAX_BYTES`]. Not cached: the caller
/// keeps the image for as long as it shows it.
pub fn qr_image(text: &str, module_px: usize) -> Option<Image> {
    let (side, bgra) = QrCode::encode_str(text)?.render(module_px);
    Some(Image::new(side as u32, side as u32, bgra))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `name version data-hex row,row,...` lines from an independent encoder (the `qrcode`
    /// crate, byte mode forced, level M, smallest version).
    const VECTORS: &str = include_str!("qr_vectors.txt");

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn matrices_match_the_reference_encoder() {
        let mut n = 0;
        for line in VECTORS.lines().filter(|l| !l.trim().is_empty()) {
            let mut f = line.split(' ');
            let (name, version) = (f.next().unwrap(), f.next().unwrap().parse::<u8>().unwrap());
            let data = unhex(f.next().unwrap());
            let rows: Vec<&str> = f.next().unwrap().split(',').collect();
            let code = QrCode::encode(&data).unwrap_or_else(|| panic!("{name} must fit"));
            assert_eq!(code.version(), version, "{name}: version");
            assert_eq!(code.size(), rows.len(), "{name}: size");
            let ours = code.hex_rows();
            for (y, (a, b)) in ours.iter().zip(&rows).enumerate() {
                assert_eq!(a, b, "{name} (version {version}, mask {}): row {y} differs", code.mask());
            }
            n += 1;
        }
        assert!(n >= 17, "the vector file has {n} cases");
    }

    /// Every mask pattern produces a symbol whose two format-information copies (read back from
    /// the matrix) name that mask, in every version, and the version blocks (7+) read back too.
    #[test]
    fn every_mask_reads_back_from_both_format_copies() {
        for (len, version) in [(5usize, 1u8), (40, 3), (100, 6), (120, 7), (200, 10)] {
            let data: Vec<u8> = (0..len).map(|i| b'a' + (i * 7 % 26) as u8).collect();
            for mask in 0..8u8 {
                let c = QrCode::encode_with(&data, Some(mask)).unwrap();
                assert_eq!((c.version(), c.mask()), (version, mask));
                let n = c.size();
                let b = |x: usize, y: usize| c.get(x, y) as u32;
                let mut first = 0u32;
                for i in 0..6 {
                    first |= b(8, i) << i;
                }
                first |= b(8, 7) << 6 | b(8, 8) << 7 | b(7, 8) << 8;
                for i in 9..15 {
                    first |= b(14 - i, 8) << i;
                }
                let mut second = 0u32;
                for i in 0..8 {
                    second |= b(n - 1 - i, 8) << i;
                }
                for i in 8..15 {
                    second |= b(8, n - 15 + i) << i;
                }
                assert_eq!((first, second), (format_bits(mask), format_bits(mask)), "version {version} mask {mask}");
                if let Some(v) = version_bits(version) {
                    let mut got = 0u32;
                    for i in 0..18 {
                        got |= b(n - 11 + i % 3, i / 3) << i;
                    }
                    assert_eq!(got, v);
                }
            }
        }
    }

    /// The automatic choice is never worse than any other mask by its own penalty score.
    #[test]
    fn the_chosen_mask_has_the_lowest_penalty() {
        let data = b"https://moku.example/some/path?with=query&and=more";
        let chosen = QrCode::encode(data).unwrap();
        let score = |c: &QrCode| penalty(&(0..c.size() * c.size()).map(|i| c.get(i % c.size(), i / c.size())).collect::<Vec<_>>(), c.size());
        let best = score(&chosen);
        for mask in 0..8u8 {
            assert!(score(&QrCode::encode_with(data, Some(mask)).unwrap()) >= best, "mask {mask}");
        }
    }

    #[test]
    fn capacities_and_limits() {
        let caps: Vec<usize> = (1..=10).map(capacity).collect();
        assert_eq!(caps, [14, 26, 42, 62, 84, 106, 122, 152, 180, 213]);
        assert_eq!(MAX_BYTES, 213);
        assert_eq!(QrCode::encode(&[b'a'; 14]).unwrap().version(), 1);
        assert_eq!(QrCode::encode(&[b'a'; 15]).unwrap().version(), 2);
        assert_eq!(QrCode::encode(&[b'a'; 213]).unwrap().version(), 10);
        assert!(QrCode::encode(&[b'a'; 214]).is_none());
        assert_eq!(QrCode::encode(b"").unwrap().size(), 21);
    }

    /// The tutorial example every QR text uses: "HELLO WORLD" in version 1-M has these ten EC
    /// codewords after the data codewords 32 91 11 120 209 114 220 77 67 64 236 17 ...
    #[test]
    fn reed_solomon_known_codewords() {
        let data = [32u8, 91, 11, 120, 209, 114, 220, 77, 67, 64, 236, 17, 236, 17, 236, 17];
        assert_eq!(reed_solomon(&data, 10), [196, 35, 39, 119, 235, 215, 231, 226, 93, 23]);
    }

    /// ISO 18004 tables: the format words for level M and the version words.
    #[test]
    fn format_and_version_information_words() {
        let m: Vec<u32> = (0..8).map(format_bits).collect();
        assert_eq!(
            m,
            [0b101010000010010, 0b101000100100101, 0b101111001111100, 0b101101101001011, 0b100010111111001, 0b100000011001110, 0b100111110010111, 0b100101010100000]
        );
        assert_eq!(version_bits(7), Some(0b000111110010010100));
        assert_eq!(version_bits(8), Some(0b001000010110111100));
        assert_eq!(version_bits(9), Some(0b001001101010011001));
        assert_eq!(version_bits(10), Some(0b001010010011010011));
        assert_eq!(version_bits(6), None);
    }

    #[test]
    fn data_codewords_of_a_short_text() {
        // "A" in version 1: 0100 00000001 01000001, terminator, then 0xEC 0x11 padding.
        let d = data_codewords(1, b"A");
        assert_eq!(d.len(), 16);
        assert_eq!(&d[..4], &[0x40, 0x14, 0x10, 0xEC]);
        assert_eq!(d[4], 0x11);
    }

    #[test]
    fn function_patterns_are_in_place() {
        let c = QrCode::encode(b"https://example.com").unwrap();
        let n = c.size();
        // Finder centres and corners, the timing pattern, the dark module.
        assert!(c.get(0, 0) && c.get(3, 3) && c.get(n - 1, 0) && c.get(0, n - 1));
        assert!(!c.get(7, 7) && !c.get(1, 1), "light ring / separator");
        assert!(c.get(8, 6) && !c.get(9, 6) && c.get(10, 6));
        assert!(c.get(8, n - 8));
        assert!(!c.get(n, 0) && !c.get(0, n), "outside is light");
    }

    #[test]
    fn rendering_adds_the_quiet_zone() {
        let c = QrCode::encode(b"x").unwrap();
        let (side, px) = c.render(2);
        assert_eq!(side, (21 + 8) * 2);
        assert_eq!(px.len(), side * side * 4);
        assert_eq!(&px[..4], &[255, 255, 255, 255], "quiet zone is white");
        // The top-left finder module is black.
        let i = ((QUIET * 2) * side + QUIET * 2) * 4;
        assert_eq!(&px[i..i + 4], &[0, 0, 0, 255]);
    }

    #[test]
    fn the_widget_caches_by_text_and_sizes_to_fit() {
        clear_cache();
        let a = qr("https://example.com");
        let b = qr("https://example.com");
        assert!(a == b, "the same bitmap is reused, so the node compares equal and nothing is redrawn");
        let c = qr("https://example.org");
        assert!(a != c);
        // Too long: a caption, not an image.
        let long = "x".repeat(300);
        assert!(qr(&long) != a);
        clear_cache();
    }

    // ---- a decoder, written from the standard, to check the encoder against ---------------------
    //
    // Ported from the phone-transfer track's own encoder (v0.3 track R), which was merged into this
    // one: the reference-matrix test above proves we write what a trusted encoder writes; this
    // reads a symbol back the way a scanner would (format information, zigzag, unmask,
    // de-interleave, Reed-Solomon syndromes, byte-mode header) and proves it says what was put in.

    fn bit(q: &QrCode, x: usize, y: usize) -> u32 {
        q.get(x, y) as u32
    }

    /// Read the format information (both copies must agree) -> the mask.
    fn read_format(q: &QrCode) -> Result<u8, String> {
        let s = q.size();
        let mut a = 0u32;
        for i in 0..=5 {
            a |= bit(q, 8, i) << i;
        }
        a |= bit(q, 8, 7) << 6;
        a |= bit(q, 8, 8) << 7;
        a |= bit(q, 7, 8) << 8;
        for i in 9..15 {
            a |= bit(q, 14 - i, 8) << i;
        }
        let mut b = 0u32;
        for i in 0..8 {
            b |= bit(q, s - 1 - i, 8) << i;
        }
        for i in 8..15 {
            b |= bit(q, 8, s - 15 + i) << i;
        }
        if a != b {
            return Err(format!("format copies differ: {a:015b} vs {b:015b}"));
        }
        (0..8u8).find(|m| format_bits(*m) == a).ok_or_else(|| format!("format {a:015b} is not a level-M code"))
    }

    /// The mask conditions of ISO/IEC 18004 table 10 (`x` = column, `y` = row).
    fn mask_applies(mask: u8, x: usize, y: usize) -> bool {
        match mask {
            0 => (x + y) % 2 == 0,
            1 => y % 2 == 0,
            2 => x % 3 == 0,
            3 => (x + y) % 3 == 0,
            4 => (y / 2 + x / 3) % 2 == 0,
            5 => (x * y) % 2 + (x * y) % 3 == 0,
            6 => ((x * y) % 2 + (x * y) % 3) % 2 == 0,
            _ => (((x + y) % 2) + (x * y) % 3) % 2 == 0,
        }
    }

    /// Which modules are function modules, as a reader knows them from the version alone.
    fn function_map(version: u8) -> Builder {
        let mut b = Builder::new(version, 17 + 4 * version as usize);
        b.draw_function_patterns();
        b
    }

    /// Padding bits after the last codeword (ISO/IEC 18004 table 1).
    fn remainder_bits(version: u8) -> usize {
        match version {
            2..=6 => 7,
            _ => 0,
        }
    }

    /// Read the data bits (zigzag), undo the mask, return the codeword bytes.
    fn read_codewords(q: &QrCode, version: u8, mask: u8) -> Vec<u8> {
        let f = function_map(version);
        let size = q.size();
        let mut bits = vec![];
        let mut right = size as i32 - 1;
        while right >= 1 {
            if right == 6 {
                right = 5;
            }
            for vert in 0..size {
                for j in 0..2 {
                    let x = (right - j) as usize;
                    let upward = ((right + 1) & 2) == 0;
                    let y = if upward { size - 1 - vert } else { vert };
                    if !f.func[y * size + x] {
                        bits.push(q.get(x, y) ^ mask_applies(mask, x, y));
                    }
                }
            }
            right -= 2;
        }
        let (ec, na, da, nb, db) = SPECS[version as usize - 1];
        let ncw = na * da + nb * db + ec * (na + nb);
        assert_eq!(bits.len(), ncw * 8 + remainder_bits(version), "data module count for version {version}");
        bits.chunks(8).take(ncw).map(|c| c.iter().fold(0u8, |a, &b| (a << 1) | b as u8)).collect()
    }

    /// Undo the interleaving, check every block's Reed-Solomon syndromes are zero, return the data codewords.
    fn deinterleave(cw: &[u8], version: u8) -> Result<Vec<u8>, String> {
        let (ec, na, da, nb, db) = SPECS[version as usize - 1];
        let sizes: Vec<usize> = std::iter::repeat_n(da, na).chain(std::iter::repeat_n(db, nb)).collect();
        let mut blocks: Vec<Vec<u8>> = sizes.iter().map(|_| vec![]).collect();
        let mut at = 0;
        for i in 0..da.max(db) {
            for (b, &len) in sizes.iter().enumerate() {
                if i < len {
                    blocks[b].push(cw[at]);
                    at += 1;
                }
            }
        }
        for _ in 0..ec {
            for blk in blocks.iter_mut() {
                blk.push(cw[at]);
                at += 1;
            }
        }
        assert_eq!(at, cw.len());
        let (exp, log) = gf_tables();
        let mul = |a: u8, b: u8| if a == 0 || b == 0 { 0 } else { exp[log[a as usize] as usize + log[b as usize] as usize] };
        let mut data = vec![];
        for (bi, blk) in blocks.iter().enumerate() {
            // Syndromes S_i = block(alpha^i), i = 0..ec: all zero for an intact block.
            for i in 0..ec {
                let s = blk.iter().fold(0u8, |acc, &c| mul(acc, exp[i]) ^ c);
                if s != 0 {
                    return Err(format!("block {bi}: syndrome {i} is {s}"));
                }
            }
            data.extend_from_slice(&blk[..sizes[bi]]);
        }
        Ok(data)
    }

    fn parse_byte_mode(data: &[u8], version: u8) -> Result<Vec<u8>, String> {
        let mut bits = data.iter().flat_map(|&b| (0..8).rev().map(move |i| (b >> i) & 1 == 1));
        let mut take = |n: usize| -> Result<u32, String> { (0..n).try_fold(0u32, |a, _| bits.next().map(|b| (a << 1) | b as u32).ok_or_else(|| "ran out of bits".to_string())) };
        let mode = take(4)?;
        if mode != 0b0100 {
            return Err(format!("mode {mode:04b}"));
        }
        let n = take(if version >= 10 { 16 } else { 8 })? as usize;
        (0..n).map(|_| take(8).map(|v| v as u8)).collect()
    }

    fn decode(q: &QrCode) -> Result<Vec<u8>, String> {
        let s = q.size();
        if s < 21 || (s - 17) % 4 != 0 {
            return Err(format!("size {s}"));
        }
        let version = ((s - 17) / 4) as u8;
        let mask = read_format(q)?;
        let cw = read_codewords(q, version, mask);
        parse_byte_mode(&deinterleave(&cw, version)?, version)
    }

    fn noise(n: usize, seed: u32) -> Vec<u8> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                (x >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn every_version_and_every_mask_decodes_back_to_the_input() {
        for version in 1..=10u8 {
            let cap = capacity(version);
            for &len in &[cap, cap - 1, cap.saturating_sub(7).max(1)] {
                let input = noise(len, (version as usize * 31 + len) as u32);
                for mask in 0..8u8 {
                    let q = QrCode::encode_with(&input, Some(mask)).unwrap();
                    assert_eq!(q.mask(), mask);
                    let got = decode(&q).unwrap_or_else(|e| panic!("v{version} len {len} mask {mask}: {e}"));
                    assert_eq!(got, input, "v{version} len {len} mask {mask}");
                }
                // The automatic choice decodes too.
                assert_eq!(decode(&QrCode::encode(&input).unwrap()).unwrap(), input);
            }
        }
    }

    #[test]
    fn urls_and_text_round_trip() {
        for s in [
            "",
            "a",
            "http://192.168.1.23:50123/Qm9rdS1SLXBob25lLXNlbmQ/",
            "https://example.com/path?x=1&y=%20z#frag",
            "WIFI:T:WPA;S:Moku;P:secret;;",
            "Teks dengan huruf Indonesia: \u{e9}\u{e8} \u{4e2d}\u{6587} \u{1F334}",
        ] {
            let q = QrCode::encode_str(s).unwrap();
            assert_eq!(decode(&q).unwrap(), s.as_bytes(), "{s}");
            assert!((21..=57).contains(&q.size()));
        }
        // A phone-transfer address (the longest the Kirim ke HP card shows) fits a small symbol.
        let q = QrCode::encode_str("http://192.168.100.100:51234/AAECAwQFBgcICQoLDA0ODw/").unwrap();
        assert_eq!(q.version(), 4);
    }

    #[test]
    fn a_damaged_symbol_is_caught_by_the_error_correction_check() {
        // The decoder above accepts only intact blocks: flip a data module and it must notice.
        let q = QrCode::encode_str("http://192.168.1.23:50123/Qm9rdS1SLXBob25lLXNlbmQ/").unwrap();
        let n = q.size();
        let mut bad = q.clone();
        bad.modules[(n - 1) * n + (n - 1)] ^= true;
        assert!(decode(&bad).is_err());
        // Damaged format information is noticed as well.
        let mut bad = q.clone();
        bad.modules[8 * n + 1] ^= true;
        assert!(decode(&bad).is_err());
    }

    #[test]
    fn the_same_input_always_gives_the_same_symbol() {
        let a = QrCode::encode_str("http://10.0.0.2:4000/abc/").unwrap();
        assert_eq!(a, QrCode::encode_str("http://10.0.0.2:4000/abc/").unwrap());
        assert_ne!(a, QrCode::encode_str("http://10.0.0.2:4000/abd/").unwrap());
    }

    #[test]
    fn the_data_area_has_exactly_the_modules_the_codewords_need() {
        for version in 1..=10u8 {
            let free = function_map(version).func.iter().filter(|&&x| !x).count();
            let (ec, na, da, nb, db) = SPECS[version as usize - 1];
            assert_eq!(free, (na * da + nb * db + ec * (na + nb)) * 8 + remainder_bits(version), "version {version}");
        }
    }

    #[test]
    fn galois_field_basics() {
        let (exp, log) = gf_tables();
        assert_eq!((exp[0], exp[1], exp[8], exp[255]), (1, 2, 0x1D, 1), "x^8 reduces by the polynomial 0x11D");
        // Every non-zero element has an inverse: a * a^-1 = 1.
        for a in 1..=255usize {
            let inv = exp[255 - log[a] as usize] as usize;
            assert_eq!(exp[log[a] as usize + log[inv] as usize], 1, "{a}");
        }
    }

    #[test]
    fn qr_image_is_the_render_as_an_image() {
        let img = qr_image("hi", 4).unwrap();
        let (side, px) = QrCode::encode_str("hi").unwrap().render(4);
        assert_eq!((img.0.width as usize, img.0.height as usize), (side, side));
        assert_eq!(img.0.bgra, px);
        assert!(qr_image(&"x".repeat(MAX_BYTES + 1), 4).is_none());
    }
}
