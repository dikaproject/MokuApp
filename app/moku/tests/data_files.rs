//! Guards the wipe lists (`moku_core::paths::DATA_FILES` / `DATA_DIRS`): `moku-setup --deletedata`
//! and "Hapus semua data" delete only Moku's *known* files, so a data file a feature starts to
//! write without being added to the list would survive an uninstall. This reads the island's
//! sources and fails for any `data_file("<name>")` that the wipe does not know.

use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn every_data_file_the_island_names_is_known_to_the_wipe() {
    let mut files = vec![];
    rust_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    let mut seen = vec![];
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        let mut rest = text.as_str();
        while let Some(i) = rest.find("data_file(\"") {
            rest = &rest[i + "data_file(\"".len()..];
            let Some(end) = rest.find('"') else { break };
            let name = &rest[..end];
            rest = &rest[end..];
            // `data_file(` is also the helper's own definition and doc examples; real names have
            // an extension.
            if name.contains('.') {
                seen.push((name.to_string(), f.display().to_string()));
            }
        }
    }
    assert!(seen.len() >= 6, "found the data files in the sources: {seen:?}");
    for (name, file) in &seen {
        assert!(moku_core::paths::is_moku_entry(name, false), "{name} (named in {file}) is missing from moku_core::paths::DATA_FILES: an uninstall would leave it behind");
    }
}

#[test]
fn the_folders_the_island_creates_in_the_data_folder_are_known_to_the_wipe() {
    for d in [moku_core::paths::clipboard_dir(), moku_core::paths::shelf_dir(), moku_core::paths::log_file().parent().unwrap().to_path_buf()] {
        let name = d.file_name().unwrap().to_string_lossy().into_owned();
        assert!(moku_core::paths::is_moku_entry(&name, true), "{name}");
    }
}
