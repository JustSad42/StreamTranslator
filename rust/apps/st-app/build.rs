fn main() {
    // see apps/st-cli/build.rs: cuDNN is only loaded once the GPU libraries are on the search path
    println!("cargo:rustc-link-arg-bins=/DELAYLOAD:cudnn64_9.dll");
    println!("cargo:rustc-link-arg-bins=delayimp.lib");
    tauri_build::build()
}
