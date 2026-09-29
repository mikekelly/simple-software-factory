fn main() -> anyhow::Result<()> {
    if let Some(path) = ssf::path_with_local_bin(
        std::env::var_os("PATH").as_deref(),
        dirs::home_dir().as_deref(),
    ) {
        // SAFETY: no other threads exist yet; the runtime starts below.
        unsafe { std::env::set_var("PATH", path) };
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(ssf::client_main())
}
