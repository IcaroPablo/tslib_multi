//! Builds opus from the opus/ submodule with cc, mirroring the defaults of
//! opus's own CMake build: floating point, float API, no neural network
//! features. The source lists come from opus's *.mk files, so they follow the
//! submodule when it moves.
//!
//! The `dnn` feature adds the decoder's neural networks: deep PLC (lost audio)
//! and OSCE's LACE and NoLACE (low-bitrate speech). They only run when a
//! decoder's complexity asks for them (5, 6 and 7). Their trained weights are
//! not in opus's git repository; `OPUS_MODEL_DIR` names a directory holding
//! opus's model archive unpacked (dnn/*_data.c and dnn/*.h), plus a file
//! `MODEL_SHA256` with the archive's checksum, which must be the one opus's
//! autogen.sh downloads. DRED and the bandwidth extension stay off.

use std::fs;
use std::path::{Path, PathBuf};

/// The release the submodule is checked out at; a git checkout has no
/// package_version file to read it from. Update it together with the submodule.
const OPUS_VERSION: &str = "1.6.1";

/// The files listed under `variable` in one of opus's *.mk files.
fn sources(opus: &Path, mk: &str, variable: &str) -> Vec<String> {
    let text = fs::read_to_string(opus.join(mk)).unwrap_or_else(|e| panic!("{mk}: {e}"));
    let start = format!("{variable} =");
    let mut lines = text.lines().skip_while(|line| !line.starts_with(&start));
    let first = lines.next().unwrap_or_else(|| panic!("{variable} is not in {mk}"));
    // The list either continues over the following lines or is a single file
    // on the same line
    let rest = first[start.len()..].trim();
    let files: Vec<String> = if rest == "\\" {
        lines
            .map(|line| line.trim_end_matches('\\').trim())
            .take_while(|file| !file.is_empty())
            .map(String::from)
            .collect()
    } else {
        rest.split_whitespace().map(String::from).collect()
    };
    assert!(!files.is_empty(), "{variable} in {mk} is empty");
    files
}

/// `OPUS_MODEL_DIR`, checked against the model archive this opus expects.
fn model_dir(opus: &Path) -> PathBuf {
    let dir = PathBuf::from(std::env::var_os("OPUS_MODEL_DIR").unwrap_or_else(|| {
        panic!("the dnn feature needs OPUS_MODEL_DIR, the directory holding opus's model files")
    }));
    let autogen = fs::read_to_string(opus.join("autogen.sh")).expect("opus/autogen.sh");
    let expected = autogen
        .lines()
        .find_map(|line| line.strip_prefix("dnn/download_model.sh "))
        .expect("autogen.sh names no model archive")
        .trim_matches('"');
    let Ok(have) = fs::read_to_string(dir.join("MODEL_SHA256")) else {
        panic!(
            "{} has no MODEL_SHA256: unpack opus_data-{expected}.tar.gz there",
            dir.display()
        );
    };
    if have.trim() != expected {
        panic!(
            "{} holds models {:?}, but this opus needs {expected}",
            dir.display(),
            have.trim()
        );
    }
    dir
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
    let neon = target_arch == "aarch64";
    if neon {
        files.extend(sources(opus, "celt_sources.mk", "CELT_SOURCES_ARM_NEON_INTR"));
        files.extend(sources(opus, "silk_sources.mk", "SILK_SOURCES_ARM_NEON_INTR"));
        build
            // The silk NEON code includes main_FIX.h
            .include(opus.join("silk/fixed"))
            .define("OPUS_ARM_MAY_HAVE_NEON_INTR", None)
            .define("OPUS_ARM_PRESUME_NEON", None)
            .define("OPUS_ARM_PRESUME_NEON_INTR", None)
            .define("OPUS_ARM_PRESUME_AARCH64_NEON_INTR", None);
    }

    let mut model_files = Vec::new();
    println!("cargo:rerun-if-env-changed=OPUS_MODEL_DIR");
    if std::env::var_os("CARGO_FEATURE_DNN").is_some() {
        let models = model_dir(opus);
        println!("cargo:rerun-if-changed={}", models.display());
        let mut dnn = sources(opus, "lpcnet_sources.mk", "DEEP_PLC_SOURCES");
        dnn.extend(sources(opus, "lpcnet_sources.mk", "OSCE_SOURCES"));
        if neon {
            dnn.extend(sources(opus, "lpcnet_sources.mk", "DNN_SOURCES_NEON"));
        }
        for file in dnn {
            if !file.ends_with("_data.c") {
                files.push(file);
            } else if file != "dnn/bbwenet_data.c" {
                // The bandwidth extension's weights are only used with ENABLE_OSCE_BWE
                model_files.push(models.join(file));
            }
        }
        build
            .include(opus.join("dnn"))
            .include(models.join("dnn"))
            .define("ENABLE_DEEP_PLC", None)
            .define("ENABLE_OSCE", None);
    }

    build.files(files.iter().map(|file| opus.join(file)));
    build.files(model_files);
    build.compile("opus");
}
