//! The compact files the helper process (`moku.exe --launcher-index`, `--icons`) hands to the
//! island: names, targets and premultiplied BGRA icons. Pure (no Windows API), so it is tested
//! without a shell. The island reads a file into ONE buffer and keeps icons as offsets into it,
//! so dropping the index returns the whole block to the system at once.
//!
//! ```text
//! index file  : "MKIX" u16 version, u16 icon_px, u32 count, count * record, "END."
//! record      : u8 kind (0 shortcut, 1 packaged app), str name, str detail, str target, icon
//! icons file  : "MKIC" u16 version, u16 icon_px, u32 count, count * (str path, icon), "END."
//! str         : u16 length, UTF-8 bytes (at most 2048)
//! icon        : u16 w, u16 h (0, 0 = no icon), w * h * 4 bytes premultiplied BGRA
//! ```
//! All integers are little endian. A damaged or truncated file decodes to `None` (the island
//! then behaves as if there were no cache).

use std::io::Write;
use std::path::Path;

pub const ICON_PX: u32 = 48;
const VERSION: u16 = 1;
const MAGIC_INDEX: &[u8; 4] = b"MKIX";
const MAGIC_ICONS: &[u8; 4] = b"MKIC";
const END: &[u8; 4] = b"END.";
const MAX_STR: usize = 2048;
const MAX_RECORDS: u32 = 10_000;
const MAX_ICON_SIDE: u16 = 128;

/// What a launcher entry starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A Start Menu shortcut (target = the `.lnk` path).
    Link,
    /// A packaged app (target = its AppUserModelID).
    App,
}

/// An icon to write: premultiplied BGRA, `w * h * 4` bytes.
#[derive(Clone, Debug, PartialEq)]
pub struct Icon {
    pub w: u16,
    pub h: u16,
    pub bgra: Vec<u8>,
}

/// An icon that was read: where it is in the file's buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IconRef {
    off: usize,
    pub w: u16,
    pub h: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Rec<I> {
    pub kind: Kind,
    pub name: String,
    pub detail: String,
    pub target: String,
    pub icon: Option<I>,
}

/// The buffer a file was read into; the [`IconRef`]s of its records point into it.
pub struct Blob(Vec<u8>);

impl Blob {
    /// The premultiplied BGRA bytes of an icon (`w * h * 4`).
    pub fn bgra(&self, i: &IconRef) -> &[u8] {
        &self.0[i.off..i.off + i.w as usize * i.h as usize * 4]
    }
}

/// A decoded index file: the records plus the buffer their icons point into.
pub struct Index {
    pub recs: Vec<Rec<IconRef>>,
    pub blob: Blob,
}

/// A decoded icons file: `(path, icon)` pairs.
pub struct IconSet {
    pub items: Vec<(String, Option<IconRef>)>,
    pub blob: Blob,
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    let mut end = s.len().min(MAX_STR);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    out.extend_from_slice(&(end as u16).to_le_bytes());
    out.extend_from_slice(&s.as_bytes()[..end]);
}

fn put_icon(out: &mut Vec<u8>, icon: Option<&Icon>) {
    match icon.filter(|i| i.w > 0 && i.h > 0 && i.w <= MAX_ICON_SIDE && i.h <= MAX_ICON_SIDE && i.bgra.len() == i.w as usize * i.h as usize * 4) {
        Some(i) => {
            out.extend_from_slice(&i.w.to_le_bytes());
            out.extend_from_slice(&i.h.to_le_bytes());
            out.extend_from_slice(&i.bgra);
        }
        None => out.extend_from_slice(&[0, 0, 0, 0]),
    }
}

fn header(magic: &[u8; 4], count: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(1024);
    out.extend_from_slice(magic);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(ICON_PX as u16).to_le_bytes());
    out.extend_from_slice(&(count as u32).to_le_bytes());
    out
}

pub fn encode_index(recs: &[Rec<Icon>]) -> Vec<u8> {
    let mut out = header(MAGIC_INDEX, recs.len());
    for r in recs {
        out.push(match r.kind {
            Kind::Link => 0,
            Kind::App => 1,
        });
        put_str(&mut out, &r.name);
        put_str(&mut out, &r.detail);
        put_str(&mut out, &r.target);
        put_icon(&mut out, r.icon.as_ref());
    }
    out.extend_from_slice(END);
    out
}

pub fn encode_icons(items: &[(String, Option<Icon>)]) -> Vec<u8> {
    let mut out = header(MAGIC_ICONS, items.len());
    for (path, icon) in items {
        put_str(&mut out, path);
        put_icon(&mut out, icon.as_ref());
    }
    out.extend_from_slice(END);
    out
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.b.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn str(&mut self) -> Option<String> {
        let n = self.u16()? as usize;
        if n > MAX_STR {
            return None;
        }
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
    fn icon(&mut self) -> Option<Option<IconRef>> {
        let (w, h) = (self.u16()?, self.u16()?);
        if w == 0 && h == 0 {
            return Some(None);
        }
        if w == 0 || h == 0 || w > MAX_ICON_SIDE || h > MAX_ICON_SIDE {
            return None;
        }
        let off = self.pos;
        self.take(w as usize * h as usize * 4)?;
        Some(Some(IconRef { off, w, h }))
    }
    /// Magic, version and record count of a file.
    fn header(&mut self, magic: &[u8; 4]) -> Option<u32> {
        if self.take(4)? != magic || self.u16()? != VERSION {
            return None;
        }
        let _icon_px = self.u16()?;
        let n = self.u32()?;
        (n <= MAX_RECORDS).then_some(n)
    }
    fn end(&mut self) -> Option<()> {
        (self.take(4)? == END && self.pos == self.b.len()).then_some(())
    }
}

impl Index {
    pub fn decode(blob: Vec<u8>) -> Option<Index> {
        let mut r = Reader { b: &blob, pos: 0 };
        let n = r.header(MAGIC_INDEX)?;
        let mut recs = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let kind = match r.u8()? {
                0 => Kind::Link,
                1 => Kind::App,
                _ => return None,
            };
            let (name, detail, target) = (r.str()?, r.str()?, r.str()?);
            recs.push(Rec { kind, name, detail, target, icon: r.icon()? });
        }
        r.end()?;
        Some(Index { recs, blob: Blob(blob) })
    }

    pub fn bgra(&self, i: &IconRef) -> &[u8] {
        self.blob.bgra(i)
    }
}

impl IconSet {
    pub fn decode(blob: Vec<u8>) -> Option<IconSet> {
        let mut r = Reader { b: &blob, pos: 0 };
        let n = r.header(MAGIC_ICONS)?;
        let mut items = Vec::with_capacity(n as usize);
        for _ in 0..n {
            items.push((r.str()?, r.icon()?));
        }
        r.end()?;
        Some(IconSet { items, blob: Blob(blob) })
    }

    pub fn bgra(&self, i: &IconRef) -> &[u8] {
        self.blob.bgra(i)
    }
}

/// Write `bytes` to `path` so that a reader sees either the old file or the whole new one:
/// a temporary file next to it, then a rename over it.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Box-filter `src` (premultiplied BGRA, `w * h`) down to `tw * th` (never up: a target at
/// least as large as the source returns the source). Averaging premultiplied channels is the
/// right way to shrink: edges stay smooth instead of jagged, which is what makes a 48 px icon
/// drawn at 24-36 px look crisp.
pub fn downscale(src: &[u8], w: u32, h: u32, tw: u32, th: u32) -> (u32, u32, Vec<u8>) {
    if tw == 0 || th == 0 || tw >= w || th >= h || src.len() != (w * h * 4) as usize {
        return (w, h, src.to_vec());
    }
    let mut out = vec![0u8; (tw * th * 4) as usize];
    for ty in 0..th {
        let (y0, y1) = (ty * h / th, ((ty + 1) * h).div_ceil(th).min(h));
        for tx in 0..tw {
            let (x0, x1) = (tx * w / tw, ((tx + 1) * w).div_ceil(tw).min(w));
            let mut acc = [0u32; 4];
            let mut n = 0u32;
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = &src[((y * w + x) * 4) as usize..][..4];
                    for c in 0..4 {
                        acc[c] += p[c] as u32;
                    }
                    n += 1;
                }
            }
            let o = ((ty * tw + tx) * 4) as usize;
            for c in 0..4 {
                out[o + c] = ((acc[c] + n / 2) / n.max(1)) as u8;
            }
        }
    }
    (tw, th, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn icon(w: u16, h: u16, v: u8) -> Icon {
        Icon { w, h, bgra: vec![v; w as usize * h as usize * 4] }
    }

    fn recs() -> Vec<Rec<Icon>> {
        vec![
            Rec { kind: Kind::Link, name: "Visual Studio Code".into(), detail: "Code.exe".into(), target: r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\Visual Studio Code.lnk".into(), icon: Some(icon(48, 48, 7)) },
            Rec { kind: Kind::App, name: "Kalkulator 計算機".into(), detail: String::new(), target: "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".into(), icon: None },
            Rec { kind: Kind::Link, name: "Tiny".into(), detail: String::new(), target: r"C:\t.lnk".into(), icon: Some(icon(16, 16, 200)) },
        ]
    }

    #[test]
    fn an_index_round_trips_with_unicode_and_missing_icons() {
        let bytes = encode_index(&recs());
        let idx = Index::decode(bytes).expect("decodes");
        assert_eq!(idx.recs.len(), 3);
        assert_eq!((idx.recs[0].kind, idx.recs[0].name.as_str(), idx.recs[0].detail.as_str()), (Kind::Link, "Visual Studio Code", "Code.exe"));
        assert_eq!(idx.recs[1].name, "Kalkulator 計算機");
        assert_eq!(idx.recs[1].kind, Kind::App);
        assert_eq!(idx.recs[1].icon, None);
        let i0 = idx.recs[0].icon.unwrap();
        assert_eq!((i0.w, i0.h), (48, 48));
        assert!(idx.bgra(&i0).iter().all(|&b| b == 7) && idx.bgra(&i0).len() == 48 * 48 * 4);
        let i2 = idx.recs[2].icon.unwrap();
        assert!(idx.bgra(&i2).iter().all(|&b| b == 200) && idx.bgra(&i2).len() == 16 * 16 * 4);
        // An empty index is a valid file.
        assert_eq!(Index::decode(encode_index(&[])).unwrap().recs.len(), 0);
    }

    #[test]
    fn an_icons_file_round_trips() {
        let items = vec![(r"C:\a\app.exe".to_string(), Some(icon(48, 48, 9))), (r"C:\b\none.exe".to_string(), None)];
        let set = IconSet::decode(encode_icons(&items)).expect("decodes");
        assert_eq!(set.items.len(), 2);
        assert_eq!(set.items[0].0, r"C:\a\app.exe");
        assert!(set.bgra(&set.items[0].1.unwrap()).iter().all(|&b| b == 9));
        assert!(set.items[1].1.is_none());
    }

    #[test]
    fn damaged_files_decode_to_nothing_instead_of_panicking() {
        let good = encode_index(&recs());
        // Every truncation fails (the trailer is missing or a field is cut short).
        for n in (0..good.len()).step_by(97).chain([good.len() - 1, good.len() - 4]) {
            assert!(Index::decode(good[..n].to_vec()).is_none(), "truncated at {n}");
        }
        // Wrong magic / version / extra bytes / a record count that is a lie.
        let mut bad = good.clone();
        bad[0] = b'X';
        assert!(Index::decode(bad).is_none());
        let mut bad = good.clone();
        bad[4] = 9;
        assert!(Index::decode(bad).is_none(), "unknown version");
        let mut bad = good.clone();
        bad.push(0);
        assert!(Index::decode(bad).is_none(), "trailing bytes");
        let mut bad = good.clone();
        bad[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Index::decode(bad).is_none(), "absurd count");
        let mut bad = good.clone();
        bad[8..12].copy_from_slice(&4u32.to_le_bytes());
        assert!(Index::decode(bad).is_none(), "count larger than the data");
        // An icon claiming more pixels than are there.
        let mut bad = encode_index(&recs()[2..].to_vec());
        let pos = bad.windows(4).position(|w| w == [16, 0, 16, 0]).expect("icon header");
        bad[pos..pos + 4].copy_from_slice(&[128, 0, 128, 0]);
        assert!(Index::decode(bad).is_none());
        // Garbage and an index file read as an icons file.
        assert!(Index::decode(vec![]).is_none() && Index::decode(vec![0xff; 100]).is_none());
        assert!(IconSet::decode(good).is_none());
    }

    #[test]
    fn long_strings_are_cut_on_a_character_boundary() {
        let long = "é".repeat(3000);
        let r = Rec { kind: Kind::Link, name: long, detail: String::new(), target: "t".into(), icon: None };
        let idx = Index::decode(encode_index(&[r])).expect("decodes");
        assert!(idx.recs[0].name.len() <= MAX_STR && idx.recs[0].name.chars().all(|c| c == 'é'));
    }

    #[test]
    fn an_icon_with_a_wrong_size_is_dropped_not_written_corrupt() {
        let r = Rec { kind: Kind::Link, name: "x".into(), detail: String::new(), target: "t".into(), icon: Some(Icon { w: 4, h: 4, bgra: vec![0; 10] }) };
        let idx = Index::decode(encode_index(&[r])).expect("decodes");
        assert_eq!(idx.recs[0].icon, None);
    }

    #[test]
    fn the_cache_file_is_written_atomically() {
        let dir = std::env::temp_dir().join(format!("moku-pack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("sub").join("launcher-index.bin");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two!").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two!");
        assert!(!dir.join("sub").join("launcher-index.bin.tmp").exists(), "no temporary file is left");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn downscaling_averages_premultiplied_pixels() {
        // 4x4 → 2x2: a checkerboard of opaque white and transparent black averages to half.
        let mut src = vec![0u8; 4 * 4 * 4];
        for y in 0..4usize {
            for x in 0..4usize {
                if (x + y) % 2 == 0 {
                    src[(y * 4 + x) * 4..][..4].copy_from_slice(&[255, 255, 255, 255]);
                }
            }
        }
        let (w, h, out) = downscale(&src, 4, 4, 2, 2);
        assert_eq!((w, h, out.len()), (2, 2, 16));
        assert!(out.iter().all(|&b| (127..=128).contains(&b)), "{out:?}");
        // Uneven ratio: 48 → 36 covers every source pixel and stays in range.
        let solid = vec![200u8; 48 * 48 * 4];
        let (w, h, out) = downscale(&solid, 48, 48, 36, 36);
        assert_eq!((w, h), (36, 36));
        assert!(out.iter().all(|&b| b == 200), "a flat colour stays flat");
        // Never scales up; a target of the same size or a bad buffer returns the source.
        assert_eq!(downscale(&solid, 48, 48, 96, 96).0, 48);
        assert_eq!(downscale(&solid, 48, 48, 48, 48).0, 48);
        assert_eq!(downscale(&[1, 2, 3], 48, 48, 24, 24).2, vec![1, 2, 3]);
    }
}
