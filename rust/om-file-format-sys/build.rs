use std::env;
use std::path::PathBuf;
use std::process::Command;

use bindgen::EnumVariation;

struct BuildConfig {
    // target: String,
    arch: String,
    target: String,
    sysroot: Option<String>,
    is_windows: bool,
}

fn get_build_config() -> BuildConfig {
    let target = env::var("TARGET").unwrap();
    BuildConfig {
        arch: env::var("CARGO_CFG_TARGET_ARCH").unwrap(),
        sysroot: env::var("SYSROOT").ok(),
        is_windows: target.contains("windows"),
        target,
    }
}

// The NDK's clang wrapper scripts (set as CC via cargo-ndk) embed the API
// level in the target triple, but bindgen talks to libclang directly and
// needs an explicit versioned triple (e.g. "aarch64-linux-android24"), or
// libclang rejects the NDK sysroot headers with "Unversioned target triples
// are not supported!".
fn android_clang_target(target: &str) -> Option<String> {
    if !target.contains("android") {
        return None;
    }
    // Only trust env vars holding a plain numeric API level (e.g. "21"); other
    // tooling sometimes sets ANDROID_PLATFORM to an ABI name like "arm64-v8a".
    let api_level = env::var("CARGO_NDK_ANDROID_PLATFORM")
        .or_else(|_| env::var("ANDROID_PLATFORM"))
        .ok()
        .filter(|v| v.chars().all(|c| c.is_ascii_digit()) && !v.is_empty())
        .unwrap_or_else(|| "21".to_string());
    Some(format!("{}{}", target, api_level))
}

fn setup_compiler(build: &mut cc::Build) -> cc::Tool {
    cc::Build::get_compiler(&build)
}

fn configure_build_flags(build: &mut cc::Build, config: &BuildConfig, compiler: &cc::Tool) {
    // If CFLAGS is set in the environment, the build system (e.g. Conda, Nix)
    // owns the flags. The cc crate will automatically pick up CFLAGS, so we
    // must not add any conflicting -O or -march flags on top of them.
    let cflags_from_env = env::var("CFLAGS").is_ok();

    if !cflags_from_env {
        // Basic compiler flags — only applied when no environment CFLAGS exist
        build.flag("-O3");

        // --- Architecture-specific flags ---
        match config.arch.as_str() {
            "aarch64" => {
                // ARM64
                build.flag("-march=armv8-a");
            }
            "x86_64" => {
                // x86_64 Architecture
                if config.is_windows && compiler.is_like_msvc() {
                    // No special flags needed for MSVC atm
                } else {
                    // x86-64-v3 has AVX2 (2013 Haswell Architecture) and should be a safe and performant baseline
                    build.flag("-march=x86-64-v3");
                }
            }
            _ => {
                // Handle other architectures if necessary
            }
        }
    }
}

fn generate_bindings(submodule: &str, sysroot: &Option<String>, target: &str) {
    let mut builder = bindgen::Builder::default()
        // Set sysroot for bindgen if specified (for cross compilation)
        .clang_arg(
            sysroot
                .as_ref()
                .map_or("".to_string(), |s| format!("--sysroot={}", s)),
        )
        .clang_arg(format!("-I{}/include", submodule));

    if let Some(android_target) = android_clang_target(target) {
        builder = builder.clang_arg(format!("--target={}", android_target));
    }

    let bindings = builder
        .header(format!("{}/include/om_file_format.h", submodule))
        // This tells bindgen to generate Rust enums.
        // Rust enums have the downside of potentially causing UB
        // if the C code for some reason returns a value that is not defined in the enum.
        // Since we are in control of the C code, we can ensure that it only returns valid values!
        // https://mdaverde.com/posts/rust-bindgen-enum/
        .default_enum_style(EnumVariation::Rust {
            non_exhaustive: false,
        })
        .generate()
        .expect("Unable to generate bindings");

    // Write the bindings to the $OUT_DIR/bindings.rs file
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");
}

fn main() {
    const LIB_NAME: &str = "omfileformatc";
    let submodule_path = "c";

    // Check if submodule exists
    if !std::path::Path::new(submodule_path).exists() {
        panic!("Submodule not found at path: {}", submodule_path);
    }

    // Re-run build script if these files change
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed={}", submodule_path);

    let config = get_build_config();
    let mut build = cc::Build::new();
    let compiler = setup_compiler(&mut build);

    println!("cargo:compiler={:?}", compiler.path());

    // Include directories
    build.include(format!("{}/include", submodule_path));
    // Add all .c files from the submodule's src directory
    let src_path = format!("{}/src", submodule_path);
    for entry in std::fs::read_dir(&src_path).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) == Some("c") {
            build.file(path);
        }
    }

    configure_build_flags(&mut build, &config, &compiler);

    // Set sysroot if specified
    if let Some(sysroot_path) = &config.sysroot {
        build.flag(&format!("--sysroot={}", sysroot_path));
    }

    // Print compiler information
    // print_compiler_info(&build);

    // Compile the library
    build.warnings(false);
    build.compile(LIB_NAME);

    generate_bindings(submodule_path, &config.sysroot, &config.target);

    // Link the static library
    println!("cargo:rustc-link-lib=static={}", LIB_NAME);
}

// Add this function to print detailed compiler configuration
#[allow(dead_code)]
fn print_compiler_info(build: &cc::Build) {
    let compiler = build.get_compiler();

    println!("cargo:warning=Compiler Configuration:");
    println!("cargo:warning=Path: {:?}", compiler.path());
    println!("cargo:warning=Is Clang: {}", compiler.is_like_clang());
    println!("cargo:warning=Is Gnu: {}", compiler.is_like_gnu());
    println!("cargo:warning=Is MSVC: {}", compiler.is_like_msvc());
    println!("cargo:warning=Arguments: {:?}", compiler.args());

    // Print environment variables that might affect compilation
    let relevant_vars = [
        "CC",
        "CFLAGS",
        "CXXFLAGS",
        "RUSTFLAGS",
        "TARGET",
        "HOST",
        "CARGO_CFG_TARGET_ARCH",
    ];

    println!("cargo:warning=Relevant Environment Variables:");
    for var in relevant_vars {
        if let Ok(value) = env::var(var) {
            println!("cargo:warning={}={}", var, value);
        }
    }

    // Print all configured flags
    println!("cargo:warning=Configured Build Flags:");
    for arg in compiler.args() {
        if let Some(flag) = arg.to_str() {
            println!("cargo:warning=Flag: {}", flag);
        }
    }
}
