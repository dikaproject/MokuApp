//! Embeds the app icon and the version info into `mokuctl.exe` (see `build-support/winres.rs`).

#[path = "../build-support/winres.rs"]
mod winres;

fn main() {
    winres::embed("Moku command-line tool", "mokuctl");
}
