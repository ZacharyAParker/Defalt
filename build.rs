//! Stamp the executable with its icon and version.
//!
//! Without this the window icon is right but Explorer, the taskbar and the
//! Alt-Tab list all show the default Rust binary icon, because those read the
//! resource table rather than asking the running process.

fn main() {
    // The terms version lives in one place, TERMS.md; the console asks for
    // acceptance of whatever that line says when it was built.
    println!("cargo:rerun-if-changed=TERMS.md");
    let terms = std::fs::read_to_string("TERMS.md").expect("TERMS.md is missing");
    let version = terms
        .split("(terms version ")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .filter(|v| v.len() == 10 && v.bytes().enumerate().all(|(i, b)| if i == 4 || i == 7 { b == b'-' } else { b.is_ascii_digit() }))
        .expect("TERMS.md has no '(terms version YYYY-MM-DD)' line");
    println!("cargo:rustc-env=DEFALT_TERMS_VERSION={version}");

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
