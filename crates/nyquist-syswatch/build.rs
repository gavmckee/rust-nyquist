fn main() {
    generate_skeletons();
}

#[cfg(target_os = "linux")]
fn generate_skeletons() {
    use libbpf_cargo::SkeletonBuilder;
    use std::path::PathBuf;

    let out_dir  = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    // vmlinux.h lives in nyquist-ebpf; avoid duplicating the large generated file.
    let ebpf_bpf = manifest.parent().unwrap().join("nyquist-ebpf").join("bpf");
    let sw_bpf   = manifest.join("bpf");

    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let (arch_dir, arch_def) = match arch.as_str() {
        "x86_64"  => ("x86_64",  "-D__TARGET_ARCH_x86"),
        "aarch64" => ("aarch64", "-D__TARGET_ARCH_arm64"),
        other => panic!("unsupported BPF arch: {other}"),
    };
    let arch_inc = ebpf_bpf.join(arch_dir);

    let programs = [
        ("bpf/sysctl",    "syswatch_sysctl"),
        ("bpf/ethtool",   "syswatch_ethtool"),
        ("bpf/rtnetlink", "syswatch_rtnetlink"),
    ];

    for (dir, stem) in programs {
        let src = manifest.join("src").join(dir).join("mod.bpf.c");
        let dst = out_dir.join(format!("{stem}.bpf.rs"));
        println!("cargo:rerun-if-changed={}", src.display());
        SkeletonBuilder::new()
            .source(&src)
            .clang_args([
                format!("-I{}", arch_inc.display()),
                format!("-I{}", sw_bpf.display()),
                arch_def.to_string(),
                "-fno-unwind-tables".to_string(),
                "-Wall".to_string(),
                "-Werror".to_string(),
            ])
            .build_and_generate(&dst)
            .unwrap_or_else(|e| {
                // Failing silently here reuses whatever stale skeleton is in
                // OUT_DIR — shipping outdated BPF bytecode. Only allow that
                // when explicitly requested (e.g. a dev box without clang).
                if std::env::var_os("NYQUIST_ALLOW_STALE_BPF").is_some() {
                    println!("cargo:warning=skeleton build skipped for {dir}: {e}");
                } else {
                    panic!(
                        "BPF skeleton build failed for {dir}: {e}\n\
                         Install clang + libelf-dev, or set NYQUIST_ALLOW_STALE_BPF=1 \
                         to knowingly reuse a previously generated skeleton."
                    );
                }
            });
    }
    println!("cargo:rerun-if-changed={}", sw_bpf.display());
}

#[cfg(not(target_os = "linux"))]
fn generate_skeletons() {}
