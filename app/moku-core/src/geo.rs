//! Places in Indonesia: the distance between two points, which province a city is in, and BMKG's
//! spelling of provinces. Pure (no OS, no network); shared by the island (BMKG alerts) and the
//! settings app (which shows what it understood).
//!
//! The nowcast feed names a province in every alert ("Hujan Lebat disertai Petir di Kep. Riau"), so
//! the alerts that matter to the user are the ones whose province is theirs. That is the
//! `bmkg.province` setting, or, when it is empty, worked out from the city: a table of the cities
//! people actually type, then the nearest provincial capital when the position is known.

/// Mean Earth radius, km.
const EARTH_KM: f64 = 6371.0088;

/// Great-circle distance between two points in degrees, in km (haversine).
pub fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let (dp, dl) = ((lat2 - lat1).to_radians(), (lon2 - lon1).to_radians());
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * EARTH_KM * a.sqrt().min(1.0).asin()
}

/// A province: its name, capital position and the spellings that mean it (letters only, lower case).
struct Province {
    name: &'static str,
    lat: f64,
    lon: f64,
    aliases: &'static [&'static str],
}

const PROVINCES: &[Province] = &[
    Province { name: "Aceh", lat: 5.55, lon: 95.32, aliases: &["aceh", "nad", "nanggroeacehdarussalam"] },
    Province { name: "Sumatera Utara", lat: 3.59, lon: 98.67, aliases: &["sumaterautara", "sumut"] },
    Province { name: "Sumatera Barat", lat: -0.95, lon: 100.35, aliases: &["sumaterabarat", "sumbar"] },
    Province { name: "Riau", lat: 0.51, lon: 101.45, aliases: &["riau"] },
    Province { name: "Kepulauan Riau", lat: 1.07, lon: 104.03, aliases: &["kepulauanriau", "kepriau", "kriau"] },
    Province { name: "Jambi", lat: -1.61, lon: 103.61, aliases: &["jambi"] },
    Province { name: "Sumatera Selatan", lat: -2.98, lon: 104.75, aliases: &["sumateraselatan", "sumsel"] },
    Province { name: "Bengkulu", lat: -3.80, lon: 102.26, aliases: &["bengkulu"] },
    Province { name: "Lampung", lat: -5.43, lon: 105.26, aliases: &["lampung"] },
    Province { name: "Kepulauan Bangka Belitung", lat: -2.13, lon: 106.11, aliases: &["kepulauanbangkabelitung", "bangkabelitung", "kepbangkabelitung", "babel"] },
    Province { name: "DKI Jakarta", lat: -6.21, lon: 106.85, aliases: &["dkijakarta", "jakarta", "dki"] },
    Province { name: "Jawa Barat", lat: -6.91, lon: 107.61, aliases: &["jawabarat", "jabar"] },
    Province { name: "Banten", lat: -6.12, lon: 106.15, aliases: &["banten"] },
    Province { name: "Jawa Tengah", lat: -6.97, lon: 110.42, aliases: &["jawatengah", "jateng"] },
    Province { name: "DI Yogyakarta", lat: -7.80, lon: 110.36, aliases: &["diyogyakarta", "yogyakarta", "diy", "jogja"] },
    Province { name: "Jawa Timur", lat: -7.25, lon: 112.75, aliases: &["jawatimur", "jatim"] },
    Province { name: "Bali", lat: -8.65, lon: 115.22, aliases: &["bali"] },
    Province { name: "Nusa Tenggara Barat", lat: -8.58, lon: 116.12, aliases: &["nusatenggarabarat", "ntb"] },
    Province { name: "Nusa Tenggara Timur", lat: -10.17, lon: 123.61, aliases: &["nusatenggaratimur", "ntt"] },
    Province { name: "Kalimantan Barat", lat: -0.03, lon: 109.33, aliases: &["kalimantanbarat", "kalbar"] },
    Province { name: "Kalimantan Tengah", lat: -2.21, lon: 113.92, aliases: &["kalimantantengah", "kalteng"] },
    Province { name: "Kalimantan Selatan", lat: -3.32, lon: 114.59, aliases: &["kalimantanselatan", "kalsel"] },
    Province { name: "Kalimantan Timur", lat: -0.50, lon: 117.15, aliases: &["kalimantantimur", "kaltim"] },
    Province { name: "Kalimantan Utara", lat: 2.84, lon: 117.37, aliases: &["kalimantanutara", "kaltara"] },
    Province { name: "Sulawesi Utara", lat: 1.47, lon: 124.84, aliases: &["sulawesiutara", "sulut"] },
    Province { name: "Gorontalo", lat: 0.54, lon: 123.06, aliases: &["gorontalo"] },
    Province { name: "Sulawesi Tengah", lat: -0.90, lon: 119.87, aliases: &["sulawesitengah", "sulteng"] },
    Province { name: "Sulawesi Barat", lat: -2.68, lon: 118.89, aliases: &["sulawesibarat", "sulbar"] },
    Province { name: "Sulawesi Selatan", lat: -5.15, lon: 119.43, aliases: &["sulawesiselatan", "sulsel"] },
    Province { name: "Sulawesi Tenggara", lat: -3.97, lon: 122.51, aliases: &["sulawesitenggara", "sultra"] },
    Province { name: "Maluku", lat: -3.70, lon: 128.18, aliases: &["maluku"] },
    Province { name: "Maluku Utara", lat: 0.79, lon: 127.38, aliases: &["malukuutara", "malut"] },
    Province { name: "Papua", lat: -2.53, lon: 140.72, aliases: &["papua"] },
    Province { name: "Papua Barat", lat: -0.86, lon: 134.06, aliases: &["papuabarat"] },
    Province { name: "Papua Barat Daya", lat: -0.88, lon: 131.26, aliases: &["papuabaratdaya"] },
    Province { name: "Papua Selatan", lat: -8.49, lon: 140.40, aliases: &["papuaselatan"] },
    Province { name: "Papua Tengah", lat: -3.36, lon: 135.50, aliases: &["papuatengah"] },
    Province { name: "Papua Pegunungan", lat: -4.09, lon: 138.95, aliases: &["papuapegunungan"] },
];

/// Cities (and regencies, and well-known districts) people type, lower case, with their province.
/// Longer names are tried first, so "tangerang selatan" is not "tangerang".
const CITIES: &[(&str, &str)] = &[
    ("jakarta", "DKI Jakarta"),
    ("tangerang selatan", "Banten"),
    ("tangsel", "Banten"),
    ("tangerang", "Banten"),
    ("serang", "Banten"),
    ("cilegon", "Banten"),
    ("bandung", "Jawa Barat"),
    ("bekasi", "Jawa Barat"),
    ("depok", "Jawa Barat"),
    ("bogor", "Jawa Barat"),
    ("cimahi", "Jawa Barat"),
    ("cirebon", "Jawa Barat"),
    ("sukabumi", "Jawa Barat"),
    ("tasikmalaya", "Jawa Barat"),
    ("karawang", "Jawa Barat"),
    ("garut", "Jawa Barat"),
    ("purwakarta", "Jawa Barat"),
    ("subang", "Jawa Barat"),
    ("cianjur", "Jawa Barat"),
    ("indramayu", "Jawa Barat"),
    ("sumedang", "Jawa Barat"),
    ("majalengka", "Jawa Barat"),
    ("kuningan", "Jawa Barat"),
    ("pangandaran", "Jawa Barat"),
    ("lembang", "Jawa Barat"),
    ("semarang", "Jawa Tengah"),
    ("surakarta", "Jawa Tengah"),
    ("solo", "Jawa Tengah"),
    ("magelang", "Jawa Tengah"),
    ("salatiga", "Jawa Tengah"),
    ("pekalongan", "Jawa Tengah"),
    ("tegal", "Jawa Tengah"),
    ("kudus", "Jawa Tengah"),
    ("purwokerto", "Jawa Tengah"),
    ("cilacap", "Jawa Tengah"),
    ("klaten", "Jawa Tengah"),
    ("sragen", "Jawa Tengah"),
    ("jepara", "Jawa Tengah"),
    ("demak", "Jawa Tengah"),
    ("kebumen", "Jawa Tengah"),
    ("wonosobo", "Jawa Tengah"),
    ("yogyakarta", "DI Yogyakarta"),
    ("jogja", "DI Yogyakarta"),
    ("yogya", "DI Yogyakarta"),
    ("sleman", "DI Yogyakarta"),
    ("bantul", "DI Yogyakarta"),
    ("wonosari", "DI Yogyakarta"),
    ("surabaya", "Jawa Timur"),
    ("malang", "Jawa Timur"),
    ("sidoarjo", "Jawa Timur"),
    ("gresik", "Jawa Timur"),
    ("kediri", "Jawa Timur"),
    ("jember", "Jawa Timur"),
    ("banyuwangi", "Jawa Timur"),
    ("madiun", "Jawa Timur"),
    ("mojokerto", "Jawa Timur"),
    ("pasuruan", "Jawa Timur"),
    ("probolinggo", "Jawa Timur"),
    ("blitar", "Jawa Timur"),
    ("lamongan", "Jawa Timur"),
    ("tuban", "Jawa Timur"),
    ("denpasar", "Bali"),
    ("badung", "Bali"),
    ("ubud", "Bali"),
    ("kuta", "Bali"),
    ("gianyar", "Bali"),
    ("singaraja", "Bali"),
    ("tabanan", "Bali"),
    ("medan", "Sumatera Utara"),
    ("binjai", "Sumatera Utara"),
    ("pematangsiantar", "Sumatera Utara"),
    ("pematang siantar", "Sumatera Utara"),
    ("deli serdang", "Sumatera Utara"),
    ("padangsidimpuan", "Sumatera Utara"),
    ("sibolga", "Sumatera Utara"),
    ("kisaran", "Sumatera Utara"),
    ("banda aceh", "Aceh"),
    ("lhokseumawe", "Aceh"),
    ("langsa", "Aceh"),
    ("sabang", "Aceh"),
    ("bukittinggi", "Sumatera Barat"),
    ("payakumbuh", "Sumatera Barat"),
    ("padang", "Sumatera Barat"),
    ("solok", "Sumatera Barat"),
    ("pekanbaru", "Riau"),
    ("dumai", "Riau"),
    ("batam", "Kepulauan Riau"),
    ("tanjungpinang", "Kepulauan Riau"),
    ("tanjung pinang", "Kepulauan Riau"),
    ("bintan", "Kepulauan Riau"),
    ("jambi", "Jambi"),
    ("palembang", "Sumatera Selatan"),
    ("lubuklinggau", "Sumatera Selatan"),
    ("prabumulih", "Sumatera Selatan"),
    ("bengkulu", "Bengkulu"),
    ("bandar lampung", "Lampung"),
    ("lampung", "Lampung"),
    ("pangkalpinang", "Kepulauan Bangka Belitung"),
    ("pangkal pinang", "Kepulauan Bangka Belitung"),
    ("belitung", "Kepulauan Bangka Belitung"),
    ("pontianak", "Kalimantan Barat"),
    ("singkawang", "Kalimantan Barat"),
    ("palangka raya", "Kalimantan Tengah"),
    ("palangkaraya", "Kalimantan Tengah"),
    ("banjarmasin", "Kalimantan Selatan"),
    ("banjarbaru", "Kalimantan Selatan"),
    ("samarinda", "Kalimantan Timur"),
    ("balikpapan", "Kalimantan Timur"),
    ("bontang", "Kalimantan Timur"),
    ("tarakan", "Kalimantan Utara"),
    ("tanjung selor", "Kalimantan Utara"),
    ("manado", "Sulawesi Utara"),
    ("bitung", "Sulawesi Utara"),
    ("tomohon", "Sulawesi Utara"),
    ("gorontalo", "Gorontalo"),
    ("palu", "Sulawesi Tengah"),
    ("poso", "Sulawesi Tengah"),
    ("mamuju", "Sulawesi Barat"),
    ("makassar", "Sulawesi Selatan"),
    ("parepare", "Sulawesi Selatan"),
    ("palopo", "Sulawesi Selatan"),
    ("maros", "Sulawesi Selatan"),
    ("gowa", "Sulawesi Selatan"),
    ("kendari", "Sulawesi Tenggara"),
    ("baubau", "Sulawesi Tenggara"),
    ("ambon", "Maluku"),
    ("ternate", "Maluku Utara"),
    ("tidore", "Maluku Utara"),
    ("mataram", "Nusa Tenggara Barat"),
    ("lombok", "Nusa Tenggara Barat"),
    ("bima", "Nusa Tenggara Barat"),
    ("sumbawa", "Nusa Tenggara Barat"),
    ("kupang", "Nusa Tenggara Timur"),
    ("labuan bajo", "Nusa Tenggara Timur"),
    ("maumere", "Nusa Tenggara Timur"),
    ("ende", "Nusa Tenggara Timur"),
    ("jayapura", "Papua"),
    ("sentani", "Papua"),
    ("merauke", "Papua Selatan"),
    ("manokwari", "Papua Barat"),
    ("sorong", "Papua Barat Daya"),
    ("timika", "Papua Tengah"),
    ("wamena", "Papua Pegunungan"),
];

/// A province name as a comparison key: lower case letters only, "Provinsi" / "Prov." dropped,
/// "D.I." and "Daerah Istimewa" read as "DI", "Kep." as "Kepulauan".
fn key(s: &str) -> String {
    let mut t = s.to_lowercase();
    for (from, to) in [("provinsi", " "), ("prov.", " "), ("daerah istimewa", "di"), ("d.i.", "di"), ("kep.", "kepulauan ")] {
        t = t.replace(from, to);
    }
    t.chars().filter(|c| c.is_alphabetic()).collect()
}

fn find(name: &str) -> Option<&'static Province> {
    let k = key(name);
    if k.is_empty() {
        return None;
    }
    PROVINCES.iter().find(|p| p.aliases.contains(&k.as_str()) || key(p.name) == k)
}

/// The province's proper name for whatever spelling (`"Kep. Riau"` -> `"Kepulauan Riau"`).
pub fn canonical(name: &str) -> Option<&'static str> {
    find(name).map(|p| p.name)
}

/// Do two spellings name the same province?
pub fn same(a: &str, b: &str) -> bool {
    matches!((find(a), find(b)), (Some(x), Some(y)) if std::ptr::eq(x, y))
}

/// The province of a city: by name, else by the nearest provincial capital when `at` is known.
pub fn province_of_city(city: &str, at: Option<(f64, f64)>) -> Option<&'static str> {
    let mut name = city.trim().to_lowercase();
    for p in ["kota administrasi ", "kota ", "kabupaten ", "kab. ", "kab "] {
        if let Some(rest) = name.strip_prefix(p) {
            name = rest.trim().to_string();
            break;
        }
    }
    if !name.is_empty() {
        // The longest city name that is the text or inside it ("Jakarta Selatan" -> "jakarta").
        let best = CITIES.iter().filter(|(c, _)| name == *c || name.split([' ', ',']).any(|w| w == *c) || name.contains(*c)).max_by_key(|(c, _)| c.len());
        if let Some((_, p)) = best {
            return Some(p);
        }
    }
    let (lat, lon) = at?;
    PROVINCES.iter().min_by(|a, b| haversine_km(lat, lon, a.lat, a.lon).total_cmp(&haversine_km(lat, lon, b.lat, b.lon))).map(|p| p.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cities_to_provinces() {
        assert_eq!(province_of_city("Jakarta", None), Some("DKI Jakarta"));
        assert_eq!(province_of_city("Jakarta Selatan", None), Some("DKI Jakarta"));
        assert_eq!(province_of_city("Kota Bandung", None), Some("Jawa Barat"));
        assert_eq!(province_of_city("Bekasi", None), Some("Jawa Barat"));
        assert_eq!(province_of_city("Depok", None), Some("Jawa Barat"));
        assert_eq!(province_of_city("Tangerang Selatan", None), Some("Banten"));
        assert_eq!(province_of_city("Tangerang", None), Some("Banten"));
        assert_eq!(province_of_city("Kabupaten Sleman", None), Some("DI Yogyakarta"));
        assert_eq!(province_of_city("Surabaya", None), Some("Jawa Timur"));
        assert_eq!(province_of_city("Bandar Lampung", None), Some("Lampung"));
        assert_eq!(province_of_city("denpasar", None), Some("Bali"));
        assert_eq!(province_of_city("Makassar", None), Some("Sulawesi Selatan"));
        assert_eq!(province_of_city("Jayapura", None), Some("Papua"));
        assert_eq!(province_of_city("Sorong", None), Some("Papua Barat Daya"));
        // Nothing known: no guess without a position.
        assert_eq!(province_of_city("Atlantis", None), None);
        assert_eq!(province_of_city("", None), None);
        // A city that is not in the table: the nearest provincial capital.
        assert_eq!(province_of_city("Kotabaru Antah Berantah", Some((-7.30, 112.70))), Some("Jawa Timur"));
        assert_eq!(province_of_city("", Some((3.5, 98.7))), Some("Sumatera Utara"));
    }

    #[test]
    fn bmkg_spellings() {
        assert_eq!(canonical("Kep. Riau"), Some("Kepulauan Riau"));
        assert_eq!(canonical("Kep. Bangka Belitung"), Some("Kepulauan Bangka Belitung"));
        assert_eq!(canonical("DKI Jakarta"), Some("DKI Jakarta"));
        assert_eq!(canonical("Jakarta"), Some("DKI Jakarta"));
        assert_eq!(canonical("Provinsi Jawa Barat"), Some("Jawa Barat"));
        assert_eq!(canonical("D.I. Yogyakarta"), Some("DI Yogyakarta"));
        assert_eq!(canonical("Daerah Istimewa Yogyakarta"), Some("DI Yogyakarta"));
        assert_eq!(canonical("Papua Barat"), Some("Papua Barat"));
        assert_eq!(canonical("papua barat daya"), Some("Papua Barat Daya"));
        assert_eq!(canonical("Narnia"), None);
        assert!(same("Kep. Riau", "Kepulauan Riau"));
        assert!(same("jawa barat", "Jawa Barat"));
        assert!(!same("Papua", "Papua Barat"), "a province is not its neighbour");
        assert!(!same("Riau", "Kep. Riau"));
        assert!(!same("Narnia", "Narnia"), "unknown names never match");
    }

    #[test]
    fn every_city_names_a_real_province() {
        for (c, p) in CITIES {
            assert!(canonical(p).is_some_and(|n| n == *p), "{c} -> {p}");
        }
    }
}
