//! Builds opus from the opus/ submodule with cc, mirroring the defaults of
//! opus's own CMake build: floating point, float API, no neural network
//! features (deep PLC, DRED, OSCE). The source lists come from opus's *.mk
//! files, so they follow the submodule when it moves.

use std::fs;
use std::path::Path;

/// The release the submodule is checked out at; a git checkout has no
/// package_version file to read it from. Update it together with the submodule.
const OPUS_VERSION: &str = "1.6.1";

/// The files listed under `variable` in one of opus's *.mk files.
fn sources(opus: &Path, mk: &str, variable: &str) -> Vec<String> {
    let text = fs::read_to_string(opus.join(mk)).unwrap_or_else(|e| panic!("{mk}: {e}"));
    let start = format!("{variable} =");
    let mut lines = text.lines().skip_while(|line| !line.starts_with(&start));
    lines.next().unwrap_or_else(|| panic!("{variable} is not in {mk}"));
    let files: Vec<String> = lines
        .map(|line| line.trim_end_matches('\\').trim())
        .take_while(|file| !file.is_empty())
        .map(String::from)
        .collect();
    assert!(!files.is_empty(), "{variable} in {mk} is empty");
    files
}

fn main() {
    let opus = Path::new("opus");
    if !opus.join("include/opus.h").exists() {
        panic!("opus/ is empty: run git submodule update --init --recursive");
    }
    println!("cargo:rerun-if-changed=opus");

    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").unwrap() == "msvc";

    let mut files = sources(opus, "celt_sources.mk", "CELT_SOURCES");
    files.extend(sources(opus, "silk_sources.mk", "SILK_SOURCES"));
    files.extend(sources(opus, "silk_sources.mk", "SILK_SOURCES_FLOAT"));
    files.extend(sources(opus, "opus_sources.mk", "OPUS_SOURCES"));
    files.extend(sources(opus, "opus_sources.mk", "OPUS_SOURCES_FLOAT"));

    let mut build = cc::Build::new();
    build
        .include(opus.join("include"))
        .include(opus)
        .include(opus.join("celt"))
        .include(opus.join("silk"))
        .include(opus.join("silk/float"))
        .define("OPUS_BUILD", None)
        .define("PACKAGE_VERSION", format!("\"{OPUS_VERSION}\"").as_str())
        .define("DISABLE_DEBUG_FLOAT", None)
        .define("ENABLE_HARDENING", None)
        .define("HAVE_LRINT", None)
        .define("HAVE_LRINTF", None)
        .warnings(false);
    if msvc {
        build.define("USE_ALLOCA", None);
    } else {
        build
            .define("VAR_ARRAYS", None)
            .define("_FORTIFY_SOURCE", "2")
            .flag("-fstack-protector-strong");
    }

    // Every aarch64 CPU has NEON, so the NEON code is used directly, without
    // run-time detection. Other targets get the plain C code.
    if target_arch == "aarch64" {
        files.extend(sources(opus, "celt_sources.mk", "CELT_SOURCES_ARM_NEON_INTR"));
        files.extend(sources(opus, "silk_sources.mk", "SILK_SOURCES_ARM_NEON_INTR"));
        build
            // The silk NEON code includes main_FIX.h
            .include(opus.join("silk/fixed"))
            .define("OPUS_ARM_MAY_HAVE_NEON_INTR", None)
            .define("OPUS_ARM_PRESUME_NEON_INTR", None)
            .define("OPUS_ARM_PRESUME_AARCH64_NEON_INTR", None);
    }

    build.files(files.iter().map(|file| opus.join(file)));
    build.compile("opus");
}
