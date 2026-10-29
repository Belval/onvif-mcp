fn main() {
    println!("cargo:rerun-if-env-changed=CUDA_PATH");
    println!("cargo:rerun-if-env-changed=CUDAToolkit_ROOT");
    if std::env::var_os("CARGO_FEATURE_CUDA").is_some()
        && let Some(root) =
            std::env::var_os("CUDA_PATH").or_else(|| std::env::var_os("CUDAToolkit_ROOT"))
    {
        let lib = std::path::PathBuf::from(root).join("lib64");
        println!("cargo:rustc-link-search=native={}", lib.display());
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib.display());
    }
}
