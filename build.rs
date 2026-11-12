fn main() {
    println!("cargo:rerun-if-changed=logo/DontreeCrab_icon.ico");

    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("logo/DontreeCrab_icon.ico");
        if let Err(err) = resource.compile() {
            println!("cargo:warning=could not embed the DontreeCrab icon: {err}");
        }
    }
}
