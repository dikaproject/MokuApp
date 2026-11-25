//! Pure shelf model (no Windows API): the list of paths dropped on the island, dedupe, caps,
//! persistence and the small formatting helpers. The shelf stores **paths only**: it never
//! copies, moves or deletes a user's file.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Most paths the shelf holds (a drop of thousands of files cannot grow it without bound).
pub const MAX_ITEMS: usize = 200;

/// Key used to detect "the same file": separators unified, long-path prefix removed, trailing
/// separator trimmed, lower-cased (Windows paths are case-insensitive).
pub fn path_key(p: &str) -> String {
    let mut s = p.trim().replace('/', "\\");
    if let Some(r) = s.strip_prefix(r"\\?\UNC\") {
        s = format!(r"\\{r}");
    } else if let Some(r) = s.strip_prefix(r"\\?\") {
        s = r.to_string();
    }
    while s.len() > 3 && s.ends_with('\\') {
        s.pop();
    }
    if s.len() == 2 && s.ends_with(':') {
        s.push('\\');
    }
    s.to_lowercase()
}

/// File or folder name for the tile (`D:\` for a drive root).
pub fn display_name(p: &str) -> String {
    let t = p.trim_end_matches(['\\', '/']);
    match t.rsplit(['\\', '/']).next() {
        Some(n) if !n.is_empty() && !n.ends_with(':') => n.to_string(),
        _ => p.to_string(),
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShelfItem {
    pub path: String,
    /// Unix seconds when it was dropped.
    #[serde(default)]
    pub added: i64,
    /// Filled in by a background `stat`, never persisted.
    #[serde(skip)]
    pub size: Option<u64>,
    #[serde(skip)]
    pub is_dir: bool,
}

/// What a background `stat` found out about one path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stat {
    Exists { size: u64, is_dir: bool },
    /// Definitely gone (`NotFound`): the item is dropped from the shelf.
    Missing,
    /// Could not tell (access denied, network share offline…): the item stays.
    Unknown,
}

#[derive(Clone, Debug, Default)]
pub struct ShelfList {
    pub items: Vec<ShelfItem>,
}

/// What a drop did: the [`path_key`]s added (in drop order) and those that were on the shelf
/// already (each once).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dropped {
    pub added: Vec<String>,
    pub already: Vec<String>,
}

impl ShelfList {
    /// Add dropped paths (skipping empties, junk and anything already on the shelf). Returns how
    /// many were added.
    pub fn add_paths(&mut self, paths: impl IntoIterator<Item = String>, now: i64) -> usize {
        self.drop_paths(paths, now).added.len()
    }

    /// [`Self::add_paths`], telling which keys were new and which were on the shelf already.
    pub fn drop_paths(&mut self, paths: impl IntoIterator<Item = String>, now: i64) -> Dropped {
        let before: HashSet<String> = self.items.iter().map(|i| path_key(&i.path)).collect();
        let mut seen = before.clone();
        let mut out = Dropped::default();
        for p in paths {
            let p = p.trim().to_string();
            if p.is_empty() || p.contains('\0') {
                continue;
            }
            let key = path_key(&p);
            if before.contains(&key) {
                if !out.already.contains(&key) {
                    out.already.push(key);
                }
                continue;
            }
            if self.items.len() >= MAX_ITEMS || !seen.insert(key.clone()) {
                continue;
            }
            self.items.push(ShelfItem { path: p, added: now, size: None, is_dir: false });
            out.added.push(key);
        }
        out
    }

    pub fn remove(&mut self, idx: usize) {
        if idx < self.items.len() {
            self.items.remove(idx);
        }
    }

    /// Forget the items with these [`path_key`]s (paths only; the files stay where they are).
    pub fn remove_keys(&mut self, keys: &[String]) {
        self.items.retain(|i| !keys.contains(&path_key(&i.path)));
    }

    /// Apply the results of a background `stat`. Returns how many vanished items were removed.
    pub fn apply_stats(&mut self, stats: &[(String, Stat)]) -> usize {
        let before = self.items.len();
        self.items.retain_mut(|i| {
            let key = path_key(&i.path);
            match stats.iter().find(|(k, _)| *k == key).map(|(_, s)| *s) {
                Some(Stat::Exists { size, is_dir }) => {
                    i.size = Some(size);
                    i.is_dir = is_dir;
                    true
                }
                Some(Stat::Missing) => false,
                Some(Stat::Unknown) | None => true,
            }
        });
        before - self.items.len()
    }

    pub fn total_size(&self) -> u64 {
        self.items.iter().filter_map(|i| i.size).sum()
    }

    /// Read the index. Missing / corrupt → empty; duplicates and empty paths are dropped.
    pub fn load(path: &Path) -> ShelfList {
        #[derive(Deserialize, Default)]
        struct File {
            #[serde(default)]
            items: Vec<ShelfItem>,
        }
        let Ok(bytes) = std::fs::read(path) else { return ShelfList::default() };
        let f: File = serde_json::from_slice(super::super::clipboard::imgutil::strip_utf8_bom(&bytes)).unwrap_or_default();
        let mut l = ShelfList::default();
        let now = 0;
        let added: Vec<(String, i64)> = f.items.into_iter().map(|i| (i.path, i.added)).collect();
        for (p, ts) in added {
            l.add_paths([p], ts.max(now));
        }
        l
    }

    /// The index file's bytes (`App::save_later` writes them off the UI thread).
    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({ "version": 1, "items": self.items })).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        crate::persist::write_atomic(path, &self.to_json())
    }
}

/// `stat` one path (background thread). `NotFound` means the file is gone, unless even the drive
/// root is missing (removed USB stick, disconnected share): then we cannot tell, and the item stays.
pub fn stat_path(p: &str) -> Stat {
    match std::fs::metadata(p) {
        Ok(m) => Stat::Exists { size: m.len(), is_dir: m.is_dir() },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let root_present = Path::new(p).ancestors().last().is_none_or(|r| r.as_os_str().is_empty() || r.exists());
            if root_present { Stat::Missing } else { Stat::Unknown }
        }
        Err(_) => Stat::Unknown,
    }
}

/// `840 KB`, `2,1 MB` (Indonesian decimal comma) / `2.1 MB`.
pub fn fmt_size(bytes: u64, english: bool) -> String {
    const K: f64 = 1024.0;
    let b = bytes as f64;
    let s = if b < K {
        format!("{bytes} B")
    } else if b < K * K {
        format!("{:.0} KB", b / K)
    } else if b < K * K * K {
        format!("{:.1} MB", b / (K * K))
    } else {
        format!("{:.1} GB", b / (K * K * K))
    };
    if english { s } else { s.replace('.', ",") }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Document,
    Image,
    Video,
    Audio,
    Archive,
    Code,
    Folder,
    Other,
}

fn ext_of(p: &str) -> String {
    let name = p.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().unwrap_or("");
    match name.rsplit_once('.') {
        Some((stem, e)) if !stem.is_empty() => e.to_ascii_lowercase(),
        _ => String::new(),
    }
}

pub fn category(p: &str, is_dir: bool) -> Category {
    if is_dir {
        return Category::Folder;
    }
    match ext_of(p).as_str() {
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "rtf" | "odt" | "ods" | "csv" | "md" | "epub" => Category::Document,
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tif" | "tiff" | "heic" | "ico" | "svg" | "avif" => Category::Image,
        "mp4" | "mkv" | "mov" | "avi" | "webm" | "wmv" | "m4v" | "flv" => Category::Video,
        "mp3" | "wav" | "flac" | "m4a" | "ogg" | "aac" | "opus" | "wma" => Category::Audio,
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "iso" => Category::Archive,
        "rs" | "py" | "js" | "ts" | "c" | "cpp" | "h" | "cs" | "java" | "go" | "json" | "toml" | "yaml" | "yml" | "html" | "css" | "sh" | "ps1" | "bat" | "xml" => Category::Code,
        _ => Category::Other,
    }
}

/// Image files WIC can usually decode for a tile preview.
pub fn is_thumbnailable(p: &str) -> bool {
    matches!(ext_of(p).as_str(), "png" | "jpg" | "jpeg" | "gif" | "bmp" | "tif" | "tiff" | "webp" | "ico")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(x: &str) -> String {
        x.to_string()
    }

    #[test]
    fn shelf_dedupes_paths() {
        let mut l = ShelfList::default();
        let added = l.add_paths([s(r"C:\Users\ana\laporan.pdf"), s(r"c:\users\ANA\Laporan.PDF"), s("C:/Users/ana/laporan.pdf")], 100);
        assert_eq!(added, 1);
        assert_eq!(l.items.len(), 1);
        // The first spelling is kept (that is what the user dropped).
        assert_eq!(l.items[0].path, r"C:\Users\ana\laporan.pdf");

        // Dropping the same files again adds nothing; new ones are added at the end.
        assert_eq!(l.add_paths([s(r"C:\Users\ana\laporan.pdf"), s(r"D:\video\demo.mp4")], 200), 1);
        assert_eq!(l.items.len(), 2);
        assert_eq!(l.items[1].path, r"D:\video\demo.mp4");
        // Trailing separators, the long-path prefix and UNC spellings collapse too.
        assert_eq!(l.add_paths([s(r"D:\video\demo.mp4\"), s(r"\\?\D:\video\demo.mp4"), s(r"\\?\UNC\srv\share\a.txt"), s(r"\\srv\share\A.TXT")], 300), 1);
        assert_eq!(l.items.len(), 3);
        // Duplicates inside one drop are caught as well.
        assert_eq!(l.add_paths([s(r"E:\x\1.png"), s(r"E:\x\1.png")], 400), 1);
    }

    #[test]
    fn shelf_drop_reports_new_and_already_present_files() {
        let mut l = ShelfList::default();
        l.add_paths([s(r"C:\a.txt"), s(r"C:\b.png")], 1);
        let d = l.drop_paths([s(r"c:\A.TXT"), s(r"C:\new.pdf"), s(r"C:\new.pdf"), s(r"C:\b.png"), s(r"C:\a.txt"), s("")], 2);
        assert_eq!(d.added, [path_key(r"C:\new.pdf")]);
        assert_eq!(d.already, [path_key(r"C:\a.txt"), path_key(r"C:\b.png")], "each duplicate once, in drop order");
        assert_eq!(l.items.len(), 3);
    }

    #[test]
    fn shelf_path_key_normalisation() {
        assert_eq!(path_key(r"C:\A\B.txt"), path_key("c:/a/b.TXT"));
        assert_eq!(path_key(r"C:\"), path_key("C:/"));
        assert_eq!(path_key(r"C:\dir\"), path_key(r"C:\dir"));
        assert_ne!(path_key(r"C:\dir\a"), path_key(r"C:\dir\b"));
        assert_eq!(path_key(r"\\?\C:\x"), path_key(r"C:\x"));
        assert_eq!(path_key(r"\\?\UNC\srv\sh\x"), path_key(r"\\srv\sh\x"));
    }

    #[test]
    fn shelf_ignores_junk_and_caps_the_list() {
        let mut l = ShelfList::default();
        assert_eq!(l.add_paths([s(""), s("   "), s("a\0b"), s(r"C:\ok.txt")], 1), 1);
        let many: Vec<String> = (0..MAX_ITEMS + 50).map(|i| format!(r"C:\f\{i}.txt")).collect();
        l.add_paths(many, 2);
        assert_eq!(l.items.len(), MAX_ITEMS, "a drop of thousands of files cannot grow the list without bound");
    }

    #[test]
    fn shelf_remove_and_clear_keep_the_files_on_disk() {
        let dir = std::env::temp_dir().join(format!("moku-shelf-test-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("keep-me.txt");
        std::fs::write(&f, b"precious").unwrap();

        let mut l = ShelfList::default();
        l.add_paths([f.to_string_lossy().to_string()], 1);
        assert_eq!(l.items.len(), 1);
        l.remove(0);
        // The trash button forgets exactly the items that flew into it (a file dropped while
        // they were flying stays).
        l.add_paths([f.to_string_lossy().to_string(), s(r"C:\later.txt")], 3);
        l.remove_keys(&[path_key(&f.to_string_lossy())]);
        assert_eq!(l.items.iter().map(|i| i.path.as_str()).collect::<Vec<_>>(), [r"C:\later.txt"]);
        l.remove_keys(&[path_key(r"C:\later.txt")]);
        assert!(l.items.is_empty());
        assert_eq!(std::fs::read(&f).unwrap(), b"precious", "removing from the shelf never deletes the user's file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shelf_corrupt_index_is_empty_and_roundtrip_works() {
        let dir = std::env::temp_dir().join(format!("moku-shelf-test-index-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let idx = dir.join("shelf.json");
        assert!(ShelfList::load(&idx).items.is_empty());
        for junk in ["", "{", "[]", "null", "{\"items\":\"x\"}", "{\"items\":[{\"nope\":1}]}"] {
            std::fs::write(&idx, junk).unwrap();
            assert!(ShelfList::load(&idx).items.is_empty(), "{junk:?}");
        }
        let mut l = ShelfList::default();
        l.add_paths([s(r"C:\a.txt"), s(r"C:\b.txt")], 50);
        l.save(&idx).unwrap();
        let back = ShelfList::load(&idx);
        assert_eq!(back.items.iter().map(|i| i.path.as_str()).collect::<Vec<_>>(), [r"C:\a.txt", r"C:\b.txt"]);
        // Hand-edited index with a duplicate and an empty path: cleaned on load.
        std::fs::write(&idx, r#"{"items":[{"path":"C:\\a.txt","added":1},{"path":"c:\\A.TXT","added":2},{"path":"","added":3}]}"#).unwrap();
        assert_eq!(ShelfList::load(&idx).items.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shelf_stats_drop_missing_files_and_fill_sizes() {
        let mut l = ShelfList::default();
        l.add_paths([s(r"C:\here.txt"), s(r"C:\gone.txt"), s(r"C:\locked.txt"), s(r"C:\dir")], 1);
        let removed = l.apply_stats(&[
            (path_key(r"C:\here.txt"), Stat::Exists { size: 2048, is_dir: false }),
            (path_key(r"C:\gone.txt"), Stat::Missing),
            (path_key(r"C:\locked.txt"), Stat::Unknown),
            (path_key(r"C:\dir"), Stat::Exists { size: 0, is_dir: true }),
        ]);
        assert_eq!(removed, 1);
        assert_eq!(l.items.len(), 3);
        assert_eq!(l.items[0].size, Some(2048));
        assert!(l.items[2].is_dir);
        // Unknown (e.g. access denied) never removes an item.
        assert!(l.items.iter().any(|i| i.path == r"C:\locked.txt"));
        assert_eq!(l.total_size(), 2048);
    }

    #[test]
    fn shelf_stat_path_tells_gone_from_unreachable() {
        let dir = std::env::temp_dir().join(format!("moku-shelf-test-stat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.txt");
        std::fs::write(&f, b"1234").unwrap();
        assert_eq!(stat_path(&f.to_string_lossy()), Stat::Exists { size: 4, is_dir: false });
        assert!(matches!(stat_path(&dir.to_string_lossy()), Stat::Exists { is_dir: true, .. }));
        std::fs::remove_file(&f).unwrap();
        assert_eq!(stat_path(&f.to_string_lossy()), Stat::Missing);
        // A drive letter that does not exist (removed USB stick): unknown, not "gone".
        if let Some(l) = ('D'..='Z').find(|l| !Path::new(&format!("{l}:\\")).exists()) {
            assert_eq!(stat_path(&format!(r"{l}:\folder\file.txt")), Stat::Unknown);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shelf_size_formatting_matches_the_mockup() {
        // Second argument: english. Indonesian uses a decimal comma, as in the mockup ("2,1 MB").
        assert_eq!(fmt_size(840 * 1024, true), "840 KB");
        assert_eq!(fmt_size(2_200_000, true), "2.1 MB");
        assert_eq!(fmt_size(2_200_000, false), "2,1 MB");
        assert_eq!(fmt_size(12 * 1024 * 1024 + 600_000, false), "12,6 MB");
        assert_eq!(fmt_size(0, true), "0 B");
        assert_eq!(fmt_size(512, true), "512 B");
        assert_eq!(fmt_size(3 * 1024 * 1024 * 1024, true), "3.0 GB");
    }

    #[test]
    fn shelf_categories_and_names() {
        assert_eq!(category(r"C:\x\laporan-Q4.PDF", false), Category::Document);
        assert_eq!(category(r"C:\x\mockup.png", false), Category::Image);
        assert_eq!(category(r"C:\x\demo.mp4", false), Category::Video);
        assert_eq!(category(r"C:\x\lagu.flac", false), Category::Audio);
        assert_eq!(category(r"C:\x\data.zip", false), Category::Archive);
        assert_eq!(category(r"C:\x\main.rs", false), Category::Code);
        assert_eq!(category(r"C:\x\noext", false), Category::Other);
        assert_eq!(category(r"C:\x\folder.png", true), Category::Folder);
        assert!(is_thumbnailable(r"C:\x\a.JPG"));
        assert!(!is_thumbnailable(r"C:\x\a.pdf"));
        assert_eq!(display_name(r"C:\Users\ana\laporan-Q4.pdf"), "laporan-Q4.pdf");
        assert_eq!(display_name(r"D:\"), r"D:\");
        assert_eq!(display_name(r"\\srv\share\a b.txt"), "a b.txt");
    }
}
