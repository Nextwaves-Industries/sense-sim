//! Checks on the shipped corpus: every document is well formed and traceable,
//! and retrieval finds the answer to realistic questions. The questions were
//! written before the corpus existed, so they are not fitted to its wording.
//! Run with `--nocapture` to see each miss.

use knowledge::{Index, builtin_docs, doc};

const DOMAINS: [&str; 6] = ["rf", "uwb", "rfid", "antenna", "positioning", "structure"];

/// (query, expected domain, a hit must contain one of these words).
const ENGINEER: &[(&str, &str, &[&str])] = &[
    ("free space path loss formula", "rf", &["20 log", "20log", "32.4", "32.44"]),
    ("concrete wall attenuation 2.4 GHz", "rf", &["concrete"]),
    ("path loss exponent indoor factory", "rf", &["exponent"]),
    ("thermal noise floor dBm per Hz", "rf", &["-174"]),
    ("Fresnel zone clearance radius", "rf", &["fresnel"]),
    ("Rician Rayleigh fading", "rf", &["rayleigh"]),
    ("UWB maximum transmit power spectral density", "uwb", &["-41.3"]),
    ("channel 5 channel 9 center frequency bandwidth", "uwb", &["6489", "7987", "499"]),
    ("DS-TWR clock drift error", "uwb", &["drift", "ppm"]),
    ("TDoA anchor clock synchronization", "uwb", &["sync"]),
    ("antenna delay calibration", "uwb", &["delay"]),
    ("NLOS range bias detection", "uwb", &["nlos"]),
    ("GDOP anchor geometry", "uwb", &["dop"]),
    ("DW3000 receiver sensitivity", "uwb", &["dw3"]),
    ("Gen2 Q algorithm slots", "rfid", &["slot"]),
    ("session S0 S1 S2 S3 persistence", "rfid", &["s2", "session"]),
    ("ETSI 865-868 MHz ERP limit", "rfid", &["865"]),
    ("FCC 902-928 MHz EIRP 4 W", "rfid", &["902"]),
    ("Vietnam UHF RFID frequency band", "rfid", &["vietnam"]),
    ("tag IC sensitivity Impinj M800", "rfid", &["m8"]),
    ("forward link reverse link read range", "rfid", &["forward"]),
    ("tag on metal detuning", "rfid", &["metal"]),
    ("dock door portal antenna placement", "rfid", &["portal", "dock"]),
    ("circular polarization reader antenna", "rfid", &["circular"]),
    ("half wave dipole length", "antenna", &["dipole"]),
    ("rectangular patch antenna width length equations", "antenna", &["patch"]),
    ("VSWR return loss conversion", "antenna", &["vswr"]),
    ("polarization mismatch loss", "antenna", &["polari"]),
    ("Chu limit small antenna Q", "antenna", &["chu"]),
    ("array grating lobes element spacing", "antenna", &["grating"]),
    ("L-match network", "antenna", &["match"]),
    ("FDTD cell size per wavelength", "antenna", &["wavelength", "lambda"]),
    ("PML boundary clearance", "antenna", &["pml"]),
    ("FR-4 dielectric constant loss tangent", "antenna", &["fr-4", "fr4"]),
    ("BLE RSSI accuracy", "positioning", &["rssi"]),
    ("AoA constant tone extension", "positioning", &["cte", "constant tone"]),
    ("Bluetooth channel sounding", "positioning", &["channel sounding"]),
    ("802.11mc fine timing measurement", "positioning", &["ftm", "fine timing"]),
    ("Kalman filter tracking", "positioning", &["kalman"]),
    ("multilateration least squares", "positioning", &["least squares", "least-squares"]),
    ("CEP95 RMSE definition", "positioning", &["cep"]),
    ("ISO/IEC 18305 test", "positioning", &["18305"]),
    ("reach truck aisle width", "structure", &["reach"]),
    ("pallet size EUR ISO 6780", "structure", &["1200", "1.2"]),
    ("sprinkler clearance rack", "structure", &["sprinkler"]),
    ("hospital corridor width", "structure", &["corridor"]),
    ("pipe flow velocity guideline", "structure", &["velocity"]),
    ("pump affinity laws NPSH", "structure", &["npsh", "affinity"]),
    ("conveyor speed", "structure", &["conveyor"]),
    ("metersPerUnit upAxis USD", "structure", &["metersperunit", "upaxis"]),
];

/// Described in plain words, without the jargon the passage would use.
const PARAPHRASE: &[(&str, &str, &[&str])] = &[
    ("signal gets weaker through walls of a building", "rf", &["wall"]),
    ("why does the signal drop so quickly when the distance doubles", "rf", &["distance", "path loss"]),
    ("position jumps when a person stands between tag and anchor", "uwb", &["body", "human", "nlos"]),
    ("how many anchors do I need for a warehouse", "uwb", &["anchor"]),
    ("how high should anchors be mounted", "uwb", &["height", "mount"]),
    ("tags are not being read on metal shelves", "rfid", &["metal"]),
    ("reader picks up tags from the next door lane", "rfid", &["stray", "cross", "unwanted", "shield"]),
    ("too many tags at once read rate falls", "rfid", &["q ", "population", "collision", "dense"]),
    ("how far can a passive tag be read", "rfid", &["range"]),
    ("how long should my antenna be for 915 MHz", "antenna", &["915"]),
    ("antenna resonance shifted after adding a plastic cover", "antenna", &["radome", "plastic", "enclosure", "cover"]),
    ("simulation result changes when I refine the grid", "antenna", &["mesh", "convergence"]),
    ("which positioning technology should I choose", "positioning", &["uwb"]),
    ("smooth noisy position estimates", "positioning", &["kalman", "filter"]),
    ("how wide should the lane between shelves be", "structure", &["aisle"]),
    ("how tall is a typical warehouse building", "structure", &["height"]),
    ("how big is a forklift", "structure", &["forklift", "truck"]),
];

/// Written before the antenna practice and RF hardware passages existed.
const DESIGN: &[(&str, &str, &[&str])] = &[
    ("which antenna type for a wearable 2.4 GHz tag", "antenna", &["pifa", "chip", "ifa", "wearable"]),
    ("resonance too low how much to shorten the antenna", "antenna", &["resonan"]),
    ("input resistance too high patch feed position", "antenna", &["feed", "inset", "probe"]),
    ("ground plane clearance keep-out chip antenna", "antenna", &["clearance", "keep-out", "keepout"]),
    ("balun common mode current on feed cable", "antenna", &["balun", "common"]),
    ("envelope correlation coefficient MIMO isolation", "antenna", &["correlation", "ecc"]),
    ("VNA calibration antenna measurement", "antenna", &["calibration", "vna"]),
    ("simulated and measured S11 disagree", "antenna", &["measure"]),
    ("good return loss but low gain", "antenna", &["gain"]),
    ("how to get circular polarisation from a patch", "antenna", &["circular", "polari"]),
    ("microstrip width for 50 ohm on FR-4", "rf", &["50", "microstrip"]),
    ("Smith chart reading", "rf", &["smith"]),
    ("coax cable loss per metre LMR-400", "rf", &["lmr"]),
    ("U.FL connector mating cycles", "rf", &["u.fl", "connector"]),
    ("inductor self resonant frequency Q at RF", "rf", &["self", "resonan"]),
    ("Friis noise figure cascade", "rf", &["cascade", "friis"]),
    ("third order intercept intermodulation", "rf", &["ip3", "intermod"]),
    ("ESD protection on antenna port", "rf", &["esd"]),
    ("RF exposure limit MPE distance ICNIRP", "rf", &["icnirp", "mpe"]),
    ("SAW filter insertion loss coexistence", "rf", &["filter"]),
];

const VIETNAMESE: &[(&str, &str, &[&str])] = &[
    ("suy hao qua tường bê tông", "rf", &["concrete"]),
    ("vị trí đặt anchor UWB", "uwb", &["anchor"]),
    ("sai số TDoA", "uwb", &["tdoa"]),
    ("thẻ RFID trên kim loại", "rfid", &["metal"]),
    ("quy định tần số RFID Việt Nam", "rfid", &["vietnam"]),
    ("độ lợi ăng ten", "antenna", &["gain"]),
    ("phân cực ăng ten", "antenna", &["polari"]),
    ("kệ hàng nhà kho", "structure", &["rack"]),
    ("lối đi xe nâng", "structure", &["aisle"]),
    ("chiều cao mái nhà xưởng", "structure", &["eave", "height", "roof"]),
];

fn index() -> Index {
    Index::build(builtin_docs())
}

/// Share of questions with an answering passage in the top 3; prints the misses.
fn recall(ix: &Index, set: &[(&str, &str, &[&str])], label: &str) -> f32 {
    let mut found = 0;
    for (query, domain, words) in set {
        let hits = ix.search(query, &[], 3, 2);
        let ok = hits.iter().any(|h| {
            let (d, c) = ix.chunk(h);
            let text = format!("{} {}", c.heading, c.text).to_lowercase();
            d.domain == *domain && words.iter().any(|w| text.contains(&w.to_lowercase()))
        });
        if ok {
            found += 1;
        } else {
            let got: Vec<String> = hits.iter().map(|h| ix.chunk(h).1.id.clone()).collect();
            println!("MISS [{label}] '{query}' wanted {domain}, got {got:?}");
        }
    }
    let r = found as f32 / set.len() as f32;
    println!("{label}: {found}/{} = {:.0}%", set.len(), r * 100.0);
    r
}

#[test]
fn every_domain_is_present() {
    let ix = index();
    let have: Vec<String> = ix.domains().into_iter().map(|d| d.0).collect();
    for d in DOMAINS {
        assert!(have.iter().any(|x| x == d), "domain {d} missing, have {have:?}");
    }
}

#[test]
fn documents_are_traceable() {
    let mut problems = Vec::new();
    for d in builtin_docs() {
        if d.title.is_empty() || d.tags.is_empty() {
            problems.push(format!("{}: missing title or tags", d.id));
        }
        if d.verified.is_empty() {
            problems.push(format!("{}: missing verified date", d.id));
        }
        if d.sources.is_empty() {
            problems.push(format!("{}: no sources", d.id));
        }
        if !DOMAINS.contains(&d.domain.as_str()) {
            problems.push(format!("{}: unexpected domain {}", d.id, d.domain));
        }
        if !d.chunks.iter().any(|c| c.heading.starts_with("Applying this in Sense Sim")) {
            problems.push(format!("{}: no 'Applying this in Sense Sim' section", d.id));
        }
        if !d.sources.iter().any(|(_, s)| s.contains("http")) {
            problems.push(format!("{}: no source has a URL", d.id));
        }
        for c in &d.chunks {
            for n in doc::citations(&c.text) {
                if !d.sources.iter().any(|(k, _)| *k == n) {
                    problems.push(format!("{}: cites [{n}] which is not in Sources", c.id));
                }
            }
        }
    }
    assert!(problems.is_empty(), "{} problems:\n{}", problems.len(), problems.join("\n"));
}

/// Tools a passage tells the assistant to use must exist in the app.
#[test]
fn named_tools_exist() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src");
    let code: String = ["rtls/tools.rs", "plant/tools.rs", "agent/tools.rs", "antenna/tools.rs", "antenna/gate.rs", "antenna/reconfig.rs", "antenna/assist.rs", "antenna/tagjob.rs", "outcomes/tools.rs"].iter().map(|f| std::fs::read_to_string(src.join(f)).unwrap()).collect();
    let mut problems = Vec::new();
    for d in builtin_docs() {
        for c in &d.chunks {
            for word in c.text.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_')) {
                if !["antenna_gain_dbi", "antenna_core"].contains(&word) && ["rtls_", "plant_", "antenna_", "design_", "site_"].iter().any(|p| word.starts_with(p)) && !code.contains(&format!("\"{word}\"")) {
                    problems.push(format!("{}: {word}", c.id));
                }
            }
        }
    }
    assert!(problems.is_empty(), "{} unknown tools:\n{}", problems.len(), problems.join("\n"));
}

#[test]
fn chunks_fit_the_retrieval_budget() {
    let mut problems = Vec::new();
    for d in builtin_docs() {
        for c in &d.chunks {
            let words = c.text.split_whitespace().count();
            if words < 15 {
                problems.push(format!("{}: only {words} words", c.id));
            }
            if words > doc::MAX_WORDS {
                problems.push(format!("{}: {words} words", c.id));
            }
        }
    }
    assert!(problems.is_empty(), "{} problems:\n{}", problems.len(), problems.join("\n"));
}

#[test]
fn engineer_questions_are_answered() {
    let r = recall(&index(), ENGINEER, "engineer");
    assert!(r >= 0.9, "recall@3 {r}");
}

#[test]
fn plain_language_questions_are_answered() {
    let r = recall(&index(), PARAPHRASE, "paraphrase");
    assert!(r >= 0.7, "recall@3 {r}");
}

#[test]
fn vietnamese_questions_are_answered() {
    let r = recall(&index(), VIETNAMESE, "vietnamese");
    assert!(r >= 0.8, "recall@3 {r}");
}

#[test]
fn design_questions_are_answered() {
    let r = recall(&index(), DESIGN, "design");
    assert!(r >= 0.85, "recall@3 {r}");
}

#[test]
fn unrelated_messages_attach_nothing() {
    let ix = index();
    for m in [
        "make the screenshot brighter",
        "undo that",
        "thanks, looks good",
        "rotate the camera to the left",
        "delete the selected object",
        "what is the capital of France",
        "write me a poem about the sea",
        "chào bạn",
    ] {
        let a = knowledge::api::auto_context(&ix, m);
        assert!(a.is_none(), "'{m}' attached {:?}", a.map(|a| a.headings));
    }
}

#[test]
fn engineering_messages_attach_passages() {
    let ix = index();
    for m in [
        "what is the attenuation of a concrete wall at 2.4 GHz",
        "how should I place UWB anchors in a warehouse",
        "UHF RFID tag on metal is not read",
        "rectangular patch antenna design equations",
        "reach truck aisle width",
    ] {
        assert!(knowledge::api::auto_context(&ix, m).is_some(), "'{m}' attached nothing");
    }
}

#[test]
fn search_is_fast_enough_for_the_ui_thread() {
    let ix = index();
    let t = std::time::Instant::now();
    for _ in 0..20 {
        for (q, _, _) in ENGINEER.iter().take(5) {
            ix.search(q, &[], 4, 2);
        }
    }
    let per = t.elapsed().as_secs_f32() * 1000.0 / 100.0;
    println!("search: {per:.2} ms per query over {} chunks", ix.chunk_count());
    // A debug build; one 60 fps frame is 16 ms.
    assert!(per < 16.0, "{per} ms per query");
}
