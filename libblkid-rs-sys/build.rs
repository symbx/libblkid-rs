use bindgen::Builder;
use std::process::Command;

use std::{env, path::PathBuf};

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR environment variable not set"));
    let target = env::var("TARGET").expect("No TARGET defined");
    let host = env::var("HOST").expect("No HOST defined");
    println!("cargo:rerun-if-env-changed=HOST");
    println!("cargo:rerun-if-env-changed=TARGET");

    let cc = cc_for_target(&target, &host);
    let repo_url = "https://github.com/util-linux/util-linux.git";
    let clone_path = out_dir.join("util-linux-source");

    if !clone_path.exists() {
        let status = Command::new("git")
            .args(["clone", "--depth", "1", repo_url])
            .arg(&clone_path)
            .status()
            .expect("Failed to run 'git clone'. Is git installed?");

        assert!(status.success(), "git clone failed");
    }

    let libs_path = out_dir.join("lib");
    let blkid_path = libs_path.join("libblkid.a");

    if !blkid_path.exists() {
        let status = Command::new("./autogen.sh")
            .current_dir(&clone_path)
            .status()
            .expect("Failed to run autogen.sh");
        assert!(status.success(), "autogen.sh failed");

        let status = Command::new("./configure")
            .current_dir(&clone_path)
            .arg(format!("--prefix={}", out_dir.display()))
            .arg("--enable-static")
            .arg("--disable-shared")
            .arg("--disable-all-programs")
            .arg("--enable-libuuid")
            .arg("--enable-libblkid")
            .arg("--without-systemd")
            .arg("--without-ncurses")
            .arg(format!("--build={host}"))
            .arg(format!("--host={target}"))
            .arg(format!("CC={}", cc))
            .status()
            .expect("Failed to run ./configure");
        assert!(status.success(), "configure failed");

        let status = Command::new("make")
            .current_dir(&clone_path)
            .status()
            .expect("Failed to run make");
        assert!(status.success(), "make failed");

        let status = Command::new("make")
            .current_dir(&clone_path)
            .arg("install")
            .status()
            .expect("Failed to run make install");
        assert!(status.success(), "make install failed");
    }

    println!("cargo:rustc-link-search=native={}/lib", out_dir.display());
    println!("cargo:rustc-link-lib=static=blkid");
    println!("cargo:rustc-link-lib=static=uuid");

    env::set_var("PKG_CONFIG_ALLOW_CROSS", "1");
    if env::var("PKG_CONFIG").is_err() {
        env::set_var("PKG_CONFIG", "pkgconf"); 
    }

    let pc_path = format!("{}/lib/pkgconfig", out_dir.display());
    let mut pkg_config = pkg_config::Config::new();
    let pkg_config = pkg_config.atleast_version("2.33.2");
    env::set_var("PKG_CONFIG_PATH", &pc_path);
    pkg_config.statik(true);
    let libblkid = pkg_config.probe("blkid").expect("Failed to find libblkid?");

    let builder = Builder::default()
        .rust_target(env!("CARGO_PKG_RUST_VERSION").parse().expect("valid"))
        .clang_arg(format!("--target={}", target.replace("musl", "gnu")))
        .clang_args(
            libblkid
                .include_paths
                .iter()
                .map(|include| format!("-I{}", include.display())),
        )
        .header("header.h")
        .size_t_is_usize(true);

    let bindings = builder.generate().expect("Unable to generate bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings");
}

fn cc_for_target(target: &str, host: &str) -> String {
    if host == target {
        return if host.contains("musl") {
            "musl-gcc".to_string()
        } else {
            "gcc".to_string()
        }
    }

    let target_underscored = target.replace('-', "_");
    let target_specific = format!("CC_{}", target_underscored);

    if let Ok(cc) = env::var(&target_specific) {
        return cc;
    }
    if let Ok(cc) = env::var("CC") {
        return cc;
    }

    let normalized = normalize_target_for_toolchain(target);

    let candidates: Vec<String> = if target.contains("musl") {
        let musl_cc = format!("{}-gcc", normalized);
        let gnu_normalized = normalized.replace("linux-musl", "linux-gnu")
            .replace("linux-musleabihf", "linux-gnueabihf");
        let gnu_cc = format!("{}-gcc", gnu_normalized);
        vec![musl_cc, gnu_cc, "gcc".to_string()]
    } else {
        let cross_cc = format!("{}-gcc", normalized);
        vec![cross_cc, "gcc".to_string()]
    };

    for candidate in &candidates {
        if is_tool_available(candidate) {
            return candidate.clone();
        }
    }

    candidates.into_iter().next().unwrap_or_else(|| "gcc".to_string())
}

fn is_tool_available(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn normalize_target_for_toolchain(target: &str) -> String {
    // Rust triples -> real toolchain prefixes
    // armv7-unknown-linux-gnueabihf  -> arm-linux-gnueabihf
    // armv7-unknown-linux-musleabihf -> arm-linux-musleabihf
    // aarch64-unknown-linux-gnu      -> aarch64-linux-gnu
    // x86_64-unknown-linux-musl      -> x86_64-linux-musl
    target
        .replace("armv7-unknown-", "arm-")
        .replace("aarch64-unknown-", "aarch64-")
        .replace("x86_64-unknown-", "x86_64-")
        // generic fallback for other "unknown" vendors
        .replace("-unknown-", "-")
}
