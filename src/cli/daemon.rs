use super::prelude::*;

pub(super) async fn run(once: bool) -> Result<()> {
    let cfg = Config::load()?;
    let listener = if once {
        None
    } else {
        dashboard_web::bind(&cfg.dashboard).await?
    };
    dashboard_web::with_daemon(listener, run_factory(cfg, once)).await
}

pub(super) async fn run_factory(cfg: Config, once: bool) -> Result<()> {
    if cfg.vm.enabled {
        // The factory lives in the VM: start it and stay with it.
        return factory_vm::Vm::new(&cfg).supervise(&cfg).await;
    }
    if cfg.repos.is_empty() && once {
        bail!("no repositories configured; run `ssf repo add owner/name --harness claude` first");
    }
    // An install from before herdr became the default, still without a
    // `driver` line, changes driver on this upgrade: say so once, where
    // Orca is around to have been the one in use.
    if let Some(note) = cfg.driver_note()
        && std::path::Path::new(&cfg.orca.command).exists()
    {
        tracing::warn!("{note}");
    }
    let engine = engine::Engine::new(cfg).await?;
    if once {
        let mut engine = engine;
        engine.tick().await;
        return Ok(());
    }
    engine.run_forever().await
}
