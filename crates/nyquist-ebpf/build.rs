fn main() {
    generate_skeletons();
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
        ("cpu/usage",          "cpu_usage"),
        ("cpu/vtime",          "cpu_vtime"),
        ("disk/blockio",       "disk_blockio"),
        ("network/traffic",    "network_traffic"),
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
