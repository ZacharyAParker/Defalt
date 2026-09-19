//! Stamp the executable with its icon and version.
//!
//! Without this the window icon is right but Explorer, the taskbar and the
//! Alt-Tab list all show the default Rust binary icon, because those read the
//! resource table rather than asking the running process.

fn main() {
    println!("cargo:rerun-if-changed=src/engine/stretch_bridge.cpp");
    println!("cargo:rerun-if-changed=vendor/signalsmith-stretch");
    println!("cargo:rerun-if-changed=vendor/signalsmith-linear");
    cc::Build::new()
        .cpp(true)
        .std("c++14")
        .opt_level(3)
        .include("vendor/signalsmith-stretch")
        .include("vendor/signalsmith-linear/include")
        .file("src/engine/stretch_bridge.cpp")
        .warnings(false)
        .compile("defalt_stretch");
    println!("cargo:rerun-if-changed=icons/icon.ico");

    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("icons/icon.ico");
        resource.set("ProductName", "Defalt");
        resource.set("FileDescription", "Defalt");
        resource.set("LegalCopyright", "Copyright © 2026 Zachary Parker");
        if let Err(error) = resource.compile() {
            // A missing resource compiler is not a reason to fail the build;
            // the application runs perfectly well with a default icon.
            println!("cargo:warning=could not embed the icon: {error}");
        }
    }
}
