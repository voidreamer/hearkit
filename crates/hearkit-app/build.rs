fn main() {
    #[cfg(target_os = "macos")]
    {
        // The screencapturekit crate's Swift bridge links libswift_Concurrency.dylib
        // via @rpath, but cargo doesn't propagate rpath link-args from transitive
        // dependencies to the final binary. Add the paths here so dyld can find it.
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
        println!("cargo:rustc-link-arg=-Wl,-rpath,/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift-5.5/macosx");
        println!("cargo:rustc-link-arg=-Wl,-rpath,/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift/macosx");
    }
    tauri_build::build()
}
