use std::path::PathBuf;
use std::process::Command;

fn main() {
    generate_skeletons();

    let programs_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../nyquist-ebpf-programs");

    let programs_dir = match programs_dir.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            println!("cargo:warning=nyquist-ebpf-programs not found: {e}");
            write_stub_and_return();
            return;
        }
    };

    println!("cargo:rerun-if-changed={}", programs_dir.join("src").display());

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let ebpf_target = if cfg!(target_endian = "big") {
        "bpfeb-unknown-none"
    } else {
        "bpfel-unknown-none"
    };

    // Run `cargo build --release` from the BPF programs directory.
    // Its rust-toolchain.toml selects nightly and its .cargo/config.toml
    // sets the target, linker (bpf-linker), and build-std.
    //
    // Unset RUSTC / RUSTC_WORKSPACE_WRAPPER so cargo doesn't use the host
    // compiler wrapper set by the outer build — the BPF programs need nightly
    // and bpf-linker, not the host stable compiler.
    // Use an isolated target directory inside OUT_DIR to prevent races with
    // the parent workspace build writing to crates/nyquist-ebpf-programs/target/.
    let bpf_target_dir = format!("{out_dir}/bpf-target");

    let output = Command::new("cargo")
        .args([
            "build",
            "--release",
            "--target", ebpf_target,
            "-Z", "build-std=core",
            "--target-dir", &bpf_target_dir,
        ])
        .current_dir(&programs_dir)
        // Unset all variables that pin the toolchain to the outer (stable) build,
        // so rustup reads nyquist-ebpf-programs/rust-toolchain.toml and picks nightly.
        .env_remove("RUSTC")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("RUSTUP_TOOLCHAIN")
        .env_remove("CARGO")
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let binary = PathBuf::from(&bpf_target_dir)
                .join(ebpf_target)
                .join("release")
                .join("nyquist-ebpf-programs");

            let dst = format!("{out_dir}/nyquist-ebpf-programs");
            match std::fs::copy(&binary, &dst) {
                Ok(_) => {
                    println!("cargo:rustc-cfg=ebpf_available");
                    println!("cargo:warning=eBPF programs compiled successfully");
                }
                Err(e) => {
                    println!("cargo:warning=Failed to copy eBPF ELF: {e}");
                    write_stub_to(&dst);
                }
            }
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            let last = stderr.lines().rev().find(|l| !l.is_empty()).unwrap_or("?");
            println!("cargo:warning=eBPF build failed: {last}");
            println!("cargo:warning=Ensure bpf-linker is installed: cargo install bpf-linker");
            println!("cargo:warning=And rust-src is available: rustup component add rust-src --toolchain nightly");
            write_stub_and_return();
        }
        Err(e) => {
            println!("cargo:warning=Failed to spawn cargo for eBPF build: {e}");
            write_stub_and_return();
        }
    }

    fn write_stub_and_return() {
        let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
        let dst = format!("{out_dir}/nyquist-ebpf-programs");
        write_stub_to(&dst);
    }

    fn write_stub_to(dst: &str) {
        let _ = std::fs::write(dst, []);
    }
}

#[cfg(target_os = "linux")]
fn generate_skeletons() {
    use libbpf_cargo::SkeletonBuilder;
    use std::path::PathBuf;

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let bpf_dir = manifest.join("bpf");

    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let (arch_dir, arch_def) = match arch.as_str() {
        "x86_64"  => ("x86_64",  "-D__TARGET_ARCH_x86"),
        "aarch64" => ("aarch64", "-D__TARGET_ARCH_arm64"),
        other => panic!("unsupported BPF arch: {other}"),
    };
    let arch_inc = bpf_dir.join(arch_dir);

    let samplers = [
        ("tcp/packet_latency", "tcp_packet_latency"),
        ("tcp/retransmit",     "tcp_retransmit"),
    ];

    for (dir, stem) in samplers {
        let src = manifest.join("src/libbpf").join(dir).join("mod.bpf.c");
        let dst = out_dir.join(format!("{stem}.bpf.rs"));
        println!("cargo:rerun-if-changed={}", src.display());
        SkeletonBuilder::new()
            .source(&src)
            .clang_args([
                format!("-I{}", arch_inc.display()),
                format!("-I{}", bpf_dir.display()),
                arch_def.to_string(),
                "-fno-unwind-tables".to_string(),
                "-Wall".to_string(),
                "-Werror".to_string(),
            ])
            .build_and_generate(&dst)
            .unwrap_or_else(|e| {
                println!("cargo:warning=skeleton build skipped for {dir}: {e}");
            });
    }
    println!("cargo:rerun-if-changed={}", bpf_dir.display());
}

#[cfg(not(target_os = "linux"))]
fn generate_skeletons() {}
