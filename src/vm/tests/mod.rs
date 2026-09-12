use super::*;
use crate::config::RepoConfig;

fn vm() -> Vm {
    let mut cfg = Config::default();
    cfg.vm.dir = "/v".into();
    cfg.vm.name = "one".into();
    cfg.vm.vcpus = Some(2);
    cfg.vm.mem_mib = Some(4096);
    cfg.vm.data_gib = Some(20);
    Vm::new(&cfg)
}

mod core;
mod runtime;
mod sizing;
