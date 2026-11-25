//! Embeds the app icon and the version info into `moku-settings.exe` (see `build-support/winres.rs`).

#[path = "../build-support/winres.rs"]
mod winres;

fn main() {
    winres::embed("Moku Settings", "moku-settings");
}
