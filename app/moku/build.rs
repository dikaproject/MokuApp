//! Embeds the app icon and the version info into `moku.exe` (see `build-support/winres.rs`).

#[path = "../build-support/winres.rs"]
mod winres;

fn main() {
    winres::embed("Moku dynamic island", "moku");
}
