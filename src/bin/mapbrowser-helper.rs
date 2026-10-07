//! CEF subprocess entry point. Sandbox initialization must precede framework load.
fn main() {
    let args = cef::args::Args::new();
    let mut sandbox = cef::sandbox::Sandbox::new();
    sandbox.initialize(args.as_main_args());
    let loader = cef::library_loader::LibraryLoader::new(
        &std::env::current_exe().expect("executable path"),
        true,
    );
    assert!(loader.load(), "Could not load Chromium in helper");
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    let exit_code = cef::execute_process(Some(args.as_main_args()), None, std::ptr::null_mut());
    std::process::exit(exit_code.max(0));
}
