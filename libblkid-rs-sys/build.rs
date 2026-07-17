use bindgen::Builder;
use std::process::Command;

use std::collections::HashMap;
use std::path::PathBuf;

const UTIL_LINUX_GIT_DEFAULT_URL: &str = "https://github.com/util-linux/util-linux.git";
const UTIL_LINUX_GIT_URL: Option<&str> = std::option_env!("UTIL_LINUX_GIT_DEFAULT_URL");

fn main() {
    let out_dir =
        PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR environment variable not set"));
    let target = std::env::var("TARGET").expect("No TARGET defined");
    let host = std::env::var("HOST").expect("No HOST defined");
    println!("cargo:rerun-if-env-changed=HOST");
    println!("cargo:rerun-if-env-changed=TARGET");

    let cc = cc_for_target(&target, &host);
    let repo_url = UTIL_LINUX_GIT_URL.unwrap_or(UTIL_LINUX_GIT_DEFAULT_URL);
    let clone_path = out_dir.join("util-linux-source");

    println!("TARGET={}", std::env::var("TARGET").unwrap());
    println!("HOST={}", std::env::var("HOST").unwrap());
    println!(
        "BINDGEN_EXTRA_CLANG_ARGS={:?}",
        std::env::var("BINDGEN_EXTRA_CLANG_ARGS")
    );

    let mut env = HashMap::new();
    env.insert("CC", cc.clone());
    if cc.contains("zig") {
        env.insert("AR", "zig ar".into());
        env.insert("RANLIB", "zig ranlib".into());
        env.insert("NM", "zig nm".into());
    }

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
            .envs(&env)
            .current_dir(&clone_path)
            .status()
            .expect("Failed to run autogen.sh");
        assert!(status.success(), "autogen.sh failed");

        let status = Command::new("./configure")
            .current_dir(&clone_path)
            .envs(&env)
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
            .status()
            .expect("Failed to run ./configure");
        assert!(status.success(), "configure failed");

        let status = Command::new("make")
            .envs(&env)
            .current_dir(&clone_path)
            .status()
            .expect("Failed to run make");
        assert!(status.success(), "make failed");

        let status = Command::new("make")
            .envs(&env)
            .current_dir(&clone_path)
            .arg("install")
            .status()
            .expect("Failed to run make install");
        assert!(status.success(), "make install failed");
    }

    println!("cargo:rustc-link-search=native={}/lib", out_dir.display());
    println!("cargo:rustc-link-lib=static=blkid");
    println!("cargo:rustc-link-lib=static=uuid");

    std::env::set_var("PKG_CONFIG_ALLOW_CROSS", "1");
    if std::env::var("PKG_CONFIG").is_err() {
        std::env::set_var("PKG_CONFIG", "pkgconf");
    }

    let pc_path = format!("{}/lib/pkgconfig", out_dir.display());
    let mut pkg_config = pkg_config::Config::new();
    let pkg_config = pkg_config.atleast_version("2.33.2");
    std::env::set_var("PKG_CONFIG_PATH", &pc_path);
    pkg_config.statik(true);
    let libblkid = pkg_config.probe("blkid").expect("Failed to find libblkid?");

    let mut builder = Builder::default()
        .rust_target(env!("CARGO_PKG_RUST_VERSION").parse().expect("valid"))
        .clang_arg("-H")
        .clang_arg(format!("--target={}", target.replace("unknown-", "")))
        .clang_args(
            libblkid
                .include_paths
                .iter()
                .map(|include| format!("-I{}", include.display())),
        )
        .header("header.h")
        .size_t_is_usize(true);

    if target != host && cc.contains("zig") {
        let (target, mcpu) = rust_to_zig_target(&target).expect("Failed to get target");
        let paths = take_zig_include_paths(&target, mcpu.as_ref().map(|x| x.as_str()));
        for path in paths {
            let arg = format!("-isystem{}", path.display());
            builder = builder.clang_arg(arg);
        }
    }

    let bindings = builder.generate().expect("Unable to generate bindings");

    let out_path = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings");
}

fn cc_for_target(target: &str, host: &str) -> String {
    if let Ok(cc) = std::env::var("CC") {
        return cc;
    }

    let zig_available = is_tool_available("zig");
    if target != host && !zig_available {
        panic!("Cross compilation cannot be done properly without zig!");
    }

    if zig_available {
        let (zig_target, mcpu) = rust_to_zig_target(target).expect("Failed to get target");
        if let Some(mcpu) = mcpu {
            return format!("zig cc -target {} -mcpu={}", zig_target, mcpu);
        } else {
            return format!("zig cc -target {}", zig_target);
        }
    }

    if host == target {
        return if host.contains("musl") {
            "musl-gcc".to_string()
        } else {
            "gcc".to_string()
        };
    }

    let target_underscored = target.replace('-', "_");
    let target_specific = format!("CC_{}", target_underscored);

    if let Ok(cc) = std::env::var(&target_specific) {
        return cc;
    }
    if let Ok(cc) = std::env::var("CC") {
        return cc;
    }

    let normalized = normalize_target_for_toolchain(target);

    let candidates: Vec<String> = if target.contains("musl") {
        let musl_cc = format!("{}-gcc", normalized);
        let gnu_normalized = normalized
            .replace("linux-musl", "linux-gnu")
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

    candidates
        .into_iter()
        .next()
        .unwrap_or_else(|| "gcc".to_string())
}

fn is_tool_available(tool: &str) -> bool {
    Command::new(tool)
        .arg("version")
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

#[allow(dead_code)]
#[derive(Debug)]
struct RustTriple<'a> {
    arch: &'a str,
    vendor: &'a str,
    os: &'a str,
    abi: Option<&'a str>,
}

fn parse_rust_triple(triple: &str) -> Option<RustTriple<'_>> {
    let mut parts = triple.split('-');

    Some(RustTriple {
        arch: parts.next()?,
        vendor: parts.next()?,
        os: parts.next()?,
        abi: parts.next(),
    })
}

fn zig_arch(arch: &str) -> (&str, Option<&str>) {
    match arch {
        "i686" => ("x86", None),
        "armv7" => ("arm", Some("generic+v7a")),
        "armv7a" => ("arm", Some("generic+v7a")),
        "riscv64gc" => ("riscv64", None),
        other => (other, None),
    }
}

fn rust_to_zig_target(triple: &str) -> Option<(String, Option<String>)> {
    let t = parse_rust_triple(triple)?;

    let (arch, mcpu) = zig_arch(t.arch);

    Some((
        match t.abi {
            Some(abi) => format!("{arch}-{}-{abi}", t.os),
            None => format!("{arch}-{}", t.os),
        },
        mcpu.map(|x| x.to_string()),
    ))
}

fn parse_zig_include_paths(output: &str) -> Vec<PathBuf> {
    let cc1 = output
        .lines()
        .find(|line| line.contains(" \"-cc1\" ") || line.contains(" -cc1 "))
        .expect("failed to find clang -cc1 invocation");

    let mut paths = Vec::new();
    let mut words = cc1.split_whitespace();

    while let Some(word) = words.next() {
        if word == "\"-isystem\"" || word == "-isystem" {
            if let Some(path) = words.next() {
                let path = path
                    .strip_prefix("\"")
                    .map(|x| x.strip_suffix("\""))
                    .flatten()
                    .unwrap_or(path);
                paths.push(PathBuf::from(path));
            }
        }
    }

    paths
}

fn take_zig_include_paths(target: &str, mcpu: Option<&str>) -> Vec<PathBuf> {
    // zig cc -### -target aarch64-linux-musl -v -c -x c /dev/null
    let mut cmd = Command::new("zig");
    cmd.arg("cc").arg("-###").arg("-target").arg(target);
    if let Some(mcpu) = mcpu {
        cmd.arg(format!("-mcpu={}", mcpu));
    }
    cmd.arg("-v").arg("-c").arg("-x").arg("c").arg("/dev/null");
    let Ok(outputs) = cmd.output() else {
        return vec![];
    };
    assert!(outputs.status.success());
    let Ok(output) = String::from_utf8(outputs.stderr) else {
        return vec![];
    };
    parse_zig_include_paths(&output)
}
