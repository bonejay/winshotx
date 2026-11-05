fn main() {
    slint_build::compile_with_config(
        "ui/app.slint",
        slint_build::CompilerConfiguration::new().with_style("fluent".into()),
    )
    .expect("Slint UI failed to compile");
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "Snapframe");
        res.set("FileDescription", "Snapframe");
        res.set_manifest(include_str!("assets/snapframe.manifest"));
        res.compile()
            .expect("could not embed the Windows resources");
    }
}
