fn main() {
    println!("cargo:rerun-if-changed=../../../logo/MKC_LOGO.ico");

    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("../../../logo/MKC_LOGO.ico");
        if let Err(err) = resource.compile() {
            println!("cargo:warning=could not embed the MKC icon: {err}");
        }
    }
}
