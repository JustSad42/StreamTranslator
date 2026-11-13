fn main() {
    // cuDNN is linked by CTranslate2 but only loaded on first use, after st_ml::gpu::setup() has put the
    // app's downloaded GPU libraries on the search path (and only if gpu::cuda_ready() found them).
    println!("cargo:rustc-link-arg-bins=/DELAYLOAD:cudnn64_9.dll");
    println!("cargo:rustc-link-arg-bins=delayimp.lib");
}
