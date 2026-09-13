//! Client-owned names and routes for independently operated SSF factories.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Catalog {
    #[serde(default)]
    servers: BTreeMap<String, Target>,
    #[serde(skip)]
    exists: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "transport", rename_all = "lowercase", deny_unknown_fields)]
pub(crate) enum Target {
    Local {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config_dir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        state_dir: Option<String>,
    },
    Vm {
        #[serde(default = "default_runtime_name")]
        runtime_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        backend: Option<String>,
        /// Present after the legacy host `[vm]` table has been adopted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config: Option<Box<crate::config::VmConfig>>,
    },
    Ssh {
        destination: String,
    },
}

impl Target {
    pub(crate) fn transport(&self) -> &'static str {
        match self {
            Self::Local { .. } => "local",
            Self::Vm { .. } => "vm",
            Self::Ssh { .. } => "ssh",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Route {
    pub name: Option<String>,
    pub destination: Option<String>,
    pub local_context: Option<LocalContext>,
    pub vm_context: Option<SelectedVmContext>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalContext {
    pub config_dir: PathBuf,
    pub state_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct SelectedVmContext {
    pub name: String,
    pub config: crate::config::VmConfig,
}

pub(crate) const SELECTED_VM_ENV: &str = "SSF_INTERNAL_SELECTED_VM";
pub(crate) const SELECTED_TARGET_ENV: &str = "SSF_INTERNAL_SELECTED_TARGET";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct TargetIdentity {
    pub name: String,
    pub transport: String,
}

struct ServiceContext {
    route: Route,
    identity: TargetIdentity,
}

static SERVICE_CONTEXT: OnceLock<ServiceContext> = OnceLock::new();

fn default_runtime_name() -> String {
    "default".into()
}

pub(crate) fn path() -> PathBuf {
    crate::config::client_config_dir().join("servers.toml")
}

impl Catalog {
    pub(crate) fn load() -> Result<Self> {
        let path = path();
        let body = match std::fs::read_to_string(&path) {
            Ok(body) => body,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
        };
        let mut catalog: Self = toml::from_str(&body)
            .with_context(|| format!("parsing server catalog {}", path.display()))?;
        catalog.exists = true;
        catalog.validate()?;
        Ok(catalog)
    }

    fn validate(&self) -> Result<()> {
        let mut legacy_managed = Vec::new();
        let mut legacy_local = Vec::new();
        let mut vm_targets = Vec::new();
        let mut owned_vm = Vec::new();
        let mut legacy_vm = Vec::new();
        let mut vm_runtime_names: BTreeMap<&str, &str> = BTreeMap::new();
        let mut vm_ports: BTreeMap<u16, (&str, &'static str)> = BTreeMap::new();
        let mut vm_paths: Vec<(PathBuf, &str, &'static str)> = Vec::new();
        let mut config_dirs: BTreeMap<PathBuf, &str> = BTreeMap::new();
        let mut state_dirs: BTreeMap<PathBuf, &str> = BTreeMap::new();
        for (name, target) in &self.servers {
            validate_name(name)?;
            match target {
                Target::Local {
                    config_dir,
                    state_dir,
                } => match (config_dir, state_dir) {
                    (None, None) => {
                        legacy_managed.push(name.as_str());
                        legacy_local.push(name.as_str());
                    }
                    (Some(config_dir), Some(state_dir)) => {
                        let config_dir = validate_owned_dir(name, "config_dir", config_dir)?;
                        let state_dir = validate_owned_dir(name, "state_dir", state_dir)?;
                        if config_dir == state_dir {
                            bail!(
                                "server {name:?} must use different config_dir and state_dir paths"
                            );
                        }
                        insert_unique_dir(&mut config_dirs, config_dir, name, "config")?;
                        insert_unique_dir(&mut state_dirs, state_dir, name, "state")?;
                    }
                    _ => bail!(
                        "server {name:?} must set both config_dir and state_dir, or neither for the legacy local factory"
                    ),
                },
                Target::Vm {
                    runtime_name,
                    backend,
                    config,
                } => {
                    validate_runtime_name(name, runtime_name)?;
                    if let Some(backend) = backend
                        && !matches!(backend.as_str(), "firecracker" | "lima")
                    {
                        bail!(
                            "server {name:?} has unknown VM backend {backend:?}; expected `firecracker` or `lima`"
                        );
                    }
                    if let Some(config) = config {
                        if !config.enabled {
                            bail!("server {name:?} owned VM config must have enabled = true");
                        }
                        config.validate()?;
                        if runtime_name != &config.name {
                            bail!(
                                "server {name:?} runtime_name {runtime_name:?} does not match its owned VM name {:?}",
                                config.name
                            );
                        }
                        if let Some(backend) = backend {
                            let configured = config
                                .backend
                                .unwrap_or_else(crate::config::BackendKind::platform_default)
                                .to_string();
                            if backend != &configured {
                                bail!(
                                    "server {name:?} backend {backend:?} does not match its owned VM backend {configured:?}"
                                );
                            }
                        }
                        if let Some(other) = vm_runtime_names.insert(&config.name, name) {
                            bail!(
                                "servers {other:?} and {name:?} share VM runtime name {:?}",
                                config.name
                            );
                        }
                        if config.ssh_port == 0 {
                            bail!("server {name:?} VM SSH port must not be zero");
                        }
                        insert_unique_port(&mut vm_ports, config.ssh_port, name, "SSH")?;
                        let effective_backend = config
                            .backend
                            .unwrap_or_else(crate::config::BackendKind::platform_default);
                        if effective_backend == crate::config::BackendKind::Firecracker {
                            let build_port = config
                                .ssh_port
                                .checked_add(1)
                                .unwrap_or_else(|| config.ssh_port.saturating_sub(1));
                            insert_unique_port(
                                &mut vm_ports,
                                build_port,
                                name,
                                "Firecracker build",
                            )?;
                        }
                        let base = validate_owned_dir(name, "VM dir", &config.dir)?;
                        vm_paths.push((base, name, "VM directory"));
                        if effective_backend == crate::config::BackendKind::Firecracker
                            && let Some(rootfs) = &config.rootfs
                        {
                            vm_paths.push((
                                validate_owned_dir(name, "VM rootfs", rootfs)?,
                                name,
                                "VM rootfs",
                            ));
                        }
                        owned_vm.push(name.as_str());
                    } else {
                        legacy_managed.push(name.as_str());
                        legacy_vm.push(name.as_str());
                    }
                    vm_targets.push(name.as_str());
                }
                Target::Ssh { destination } => {
                    if destination.is_empty() || destination.chars().any(char::is_control) {
                        bail!("server {name:?} has an empty or invalid SSH destination");
                    }
                }
            }
        }
        if vm_targets.len() > 1 && !legacy_vm.is_empty() {
            bail!(
                "legacy VM server {} cannot coexist with another managed VM; run `ssf server migrate-vm` first",
                legacy_vm.join(", ")
            );
        }
        if legacy_managed.len() > 1 {
            bail!(
                "servers {} would share the legacy config and state paths; give local targets explicit config_dir and state_dir paths",
                legacy_managed.join(", ")
            );
        }
        if !legacy_local.is_empty() && !owned_vm.is_empty() {
            bail!(
                "legacy local server {} would share the installation-wide supervisor with managed VM {}; give the local target explicit config_dir and state_dir paths",
                legacy_local.join(", "),
                owned_vm.join(", ")
            );
        }
        for (config_dir, config_owner) in &config_dirs {
            if let Some(state_owner) = state_dirs.get(config_dir) {
                bail!(
                    "servers {config_owner:?} and {state_owner:?} use the same path as config and state: {}",
                    config_dir.display()
                );
            }
        }
        let owned_dirs: Vec<_> = config_dirs
            .iter()
            .map(|(path, owner)| (path, *owner, "config"))
            .chain(
                state_dirs
                    .iter()
                    .map(|(path, owner)| (path, *owner, "state")),
            )
            .collect();
        for (vm_path, vm_owner, vm_kind) in &vm_paths {
            for (path, owner, kind) in &owned_dirs {
                if overlaps(vm_path, path) {
                    bail!(
                        "server {vm_owner:?} {vm_kind} {} overlaps server {owner:?} {kind} directory {}",
                        vm_path.display(),
                        path.display()
                    );
                }
            }
        }
        for (index, (vm_path, vm_owner, vm_kind)) in vm_paths.iter().enumerate() {
            for (other_path, other_owner, other_kind) in &vm_paths[index + 1..] {
                if vm_owner != other_owner && overlaps(vm_path, other_path) {
                    bail!(
                        "server {vm_owner:?} {vm_kind} {} overlaps server {other_owner:?} {other_kind} {}",
                        vm_path.display(),
                        other_path.display()
                    );
                }
            }
        }
        let reserved = [crate::config::config_dir(), crate::config::state_dir()];
        for (path, owner, kind) in &owned_dirs {
            for legacy in &reserved {
                if overlaps(path, legacy) {
                    bail!(
                        "server {owner:?} {kind} directory {} overlaps legacy SSF directory {}; use a sibling path such as `ssf-factories/{owner}`",
                        path.display(),
                        legacy.display()
                    );
                }
            }
        }
        for (index, (path, owner, kind)) in owned_dirs.iter().enumerate() {
            for (other_path, other_owner, other_kind) in &owned_dirs[index + 1..] {
                if overlaps(path, other_path) {
                    bail!(
                        "server {owner:?} {kind} directory {} overlaps server {other_owner:?} {other_kind} directory {}",
                        path.display(),
                        other_path.display()
                    );
                }
            }
        }
        Ok(())
    }

    pub(crate) fn list(&self) -> impl Iterator<Item = (&str, &Target)> {
        self.servers
            .iter()
            .map(|(name, target)| (name.as_str(), target))
    }

    pub(crate) fn get(&self, name: &str) -> Option<&Target> {
        self.servers.get(name)
    }

    /// Compatibility for an installation-wide process with no explicit target.
    /// It may supervise one owned VM, but must never guess between several.
    pub(crate) fn sole_owned_vm_context() -> Result<Option<SelectedVmContext>> {
        let catalog = Self::load()?;
        let contexts = catalog
            .servers
            .iter()
            .filter_map(|(name, target)| match target {
                Target::Vm {
                    config: Some(config),
                    ..
                } => Some(SelectedVmContext {
                    name: name.clone(),
                    config: config.as_ref().clone(),
                }),
                _ => None,
            })
            .collect::<Vec<_>>();
        match contexts.as_slice() {
            [] => Ok(None),
            [context] => Ok(Some(context.clone())),
            _ => bail!(
                "multiple managed VM servers are configured; this process needs an explicit server target"
            ),
        }
    }

    /// Legacy local and VM routes must still describe the factory selected by
    /// the installation-wide config. Namespaced local routes carry their own
    /// context and deliberately do not consult that config.
    pub(crate) fn validate_execution(
        &self,
        routes: &[Route],
        config: &crate::config::Config,
    ) -> Result<()> {
        for route in routes {
            let Some(name) = route.name.as_deref() else {
                continue;
            };
            match self.servers.get(name).expect("a resolved named route") {
                Target::Local {
                    config_dir: None,
                    state_dir: None,
                } if config.vm.enabled => bail!(
                    "server {name:?} is `local`, but the existing configuration has VM mode enabled"
                ),
                Target::Vm {
                    runtime_name,
                    backend,
                    config: None,
                } => {
                    if !config.vm.enabled {
                        bail!(
                            "server {name:?} is a managed VM, but the existing configuration has VM mode disabled"
                        );
                    }
                    if runtime_name != &config.vm.name {
                        bail!(
                            "server {name:?} names VM runtime {runtime_name:?}, but the existing configuration names {:?}",
                            config.vm.name
                        );
                    }
                    if let Some(backend) = backend {
                        let configured = crate::vm::Vm::new(config).backend().to_string();
                        if backend != &configured {
                            bail!(
                                "server {name:?} selects VM backend {backend:?}, but the existing configuration selects {configured:?}"
                            );
                        }
                    }
                }
                Target::Vm {
                    config: Some(owned),
                    ..
                } => {
                    if let Some(legacy) = crate::config::Config::legacy_vm_settings()?
                        && legacy != **owned
                    {
                        bail!(
                            "server {name:?} has owned VM settings that conflict with legacy [vm]; both were retained"
                        );
                    }
                }
                Target::Local { .. } | Target::Ssh { .. } => {}
            }
        }
        Ok(())
    }

    /// Resolve explicit selectors, or apply the zero/one/many rule. With no
    /// catalog, explicit values retain their legacy meaning as raw SSH routes.
    pub(crate) fn resolve(&self, requested: Vec<String>) -> Result<Vec<Route>> {
        if !requested.is_empty() {
            if !self.exists {
                return Ok(requested
                    .into_iter()
                    .map(|destination| Route {
                        name: None,
                        destination: Some(destination),
                        local_context: None,
                        vm_context: None,
                    })
                    .collect());
            }
            return requested
                .into_iter()
                .map(|name| self.named_route(&name))
                .collect();
        }

        match self.servers.len() {
            0 => Ok(vec![Route {
                name: None,
                destination: None,
                local_context: None,
                vm_context: None,
            }]),
            1 => {
                let name = self.servers.keys().next().expect("one server");
                Ok(vec![self.named_route(name)?])
            }
            _ => bail!(
                "multiple SSF servers are configured; select one with --server:\n  {}",
                self.servers
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n  ")
            ),
        }
    }

    fn named_route(&self, name: &str) -> Result<Route> {
        let target = self.servers.get(name).with_context(|| {
            let choices = self.servers.keys().cloned().collect::<Vec<_>>().join(", ");
            if choices.is_empty() {
                format!("unknown SSF server {name:?}; no servers are configured")
            } else {
                format!("unknown SSF server {name:?}; configured servers: {choices}")
            }
        })?;
        Ok(Route {
            name: Some(name.to_owned()),
            destination: match target {
                Target::Local { .. } | Target::Vm { .. } => None,
                Target::Ssh { destination } => Some(destination.clone()),
            },
            local_context: match target {
                Target::Local {
                    config_dir: Some(config_dir),
                    state_dir: Some(state_dir),
                } => Some(LocalContext {
                    config_dir: crate::config::expand_tilde(config_dir),
                    state_dir: crate::config::expand_tilde(state_dir),
                }),
                _ => None,
            },
            vm_context: match target {
                Target::Vm {
                    config: Some(config),
                    ..
                } => Some(SelectedVmContext {
                    name: name.to_owned(),
                    config: config.as_ref().clone(),
                }),
                _ => None,
            },
        })
    }

    fn save(&self) -> Result<()> {
        let path = path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        crate::config::write_atomic(&path, toml::to_string_pretty(self)?.as_bytes(), 0o600)
            .with_context(|| format!("writing server catalog {}", path.display()))
    }

    pub(crate) fn migrate_legacy_vm(name: &str) -> Result<bool> {
        validate_name(name)?;
        let mut catalog = Self::load()?;
        let Some(legacy) = crate::config::Config::legacy_vm_settings()? else {
            return match catalog.servers.get(name) {
                Some(Target::Vm {
                    config: Some(_), ..
                }) => Ok(false),
                _ => bail!("no legacy [vm] settings to migrate"),
            };
        };
        if !legacy.enabled {
            bail!("legacy [vm] enabled is false; there is no active managed VM to migrate");
        }
        legacy.validate()?;
        match catalog.servers.get(name) {
            Some(Target::Vm {
                config: Some(owned),
                ..
            }) if owned.as_ref() == &legacy => {}
            Some(Target::Vm {
                runtime_name,
                backend,
                config: None,
            }) => {
                if runtime_name != &legacy.name {
                    bail!(
                        "server {name:?} names VM runtime {runtime_name:?}, but legacy [vm] names {:?}",
                        legacy.name
                    );
                }
                if let Some(backend) = backend {
                    let legacy_backend = legacy
                        .backend
                        .unwrap_or_else(crate::config::BackendKind::platform_default)
                        .to_string();
                    if backend != &legacy_backend {
                        bail!(
                            "server {name:?} selects backend {backend:?}, but legacy [vm] selects {legacy_backend:?}"
                        );
                    }
                }
            }
            Some(Target::Vm { .. }) => bail!(
                "server {name:?} has owned VM settings that conflict with legacy [vm]; both were retained"
            ),
            Some(other) => bail!(
                "server {name:?} is {}, not a managed VM; nothing was changed",
                other.transport()
            ),
            None => {}
        }
        catalog.servers.insert(
            name.to_owned(),
            Target::Vm {
                runtime_name: legacy.name.clone(),
                backend: legacy.backend.map(|backend| backend.to_string()),
                config: Some(Box::new(legacy.clone())),
            },
        );
        catalog.exists = true;
        catalog.validate()?;
        catalog.save()?;

        let written = Self::load()?;
        let Some(Target::Vm {
            config: Some(owned),
            ..
        }) = written.servers.get(name)
        else {
            bail!("written server catalog did not retain VM target {name:?}");
        };
        if owned.as_ref() != &legacy {
            bail!("written VM settings did not verify; legacy [vm] was retained");
        }
        crate::config::Config::remove_legacy_vm_settings(&legacy)?;
        Ok(true)
    }
}

pub(crate) fn selected_vm_context() -> Result<Option<SelectedVmContext>> {
    let Some(raw) = std::env::var_os(SELECTED_VM_ENV) else {
        return Ok(SERVICE_CONTEXT
            .get()
            .and_then(|context| context.route.vm_context.clone()));
    };
    let context: SelectedVmContext = serde_json::from_slice(raw.as_encoded_bytes())
        .context("parsing selected VM target context")?;
    Ok(Some(context))
}

pub(crate) fn selected_target_name() -> Option<String> {
    selected_target_identity()
        .ok()
        .flatten()
        .map(|identity| identity.name)
}

pub(crate) fn selected_target_identity() -> Result<Option<TargetIdentity>> {
    if let Some(raw) = std::env::var_os(SELECTED_TARGET_ENV) {
        return serde_json::from_slice(raw.as_encoded_bytes())
            .context("parsing selected server identity")
            .map(Some);
    }
    Ok(SERVICE_CONTEXT
        .get()
        .map(|context| context.identity.clone()))
}

pub(crate) fn service_local_context() -> Option<&'static LocalContext> {
    SERVICE_CONTEXT
        .get()
        .and_then(|context| context.route.local_context.as_ref())
}

pub(crate) fn service_context_is_active() -> bool {
    SERVICE_CONTEXT.get().is_some()
}

pub(crate) fn activate_service_target(name: &str) -> Result<()> {
    let catalog = Catalog::load()?;
    let route = catalog.resolve(vec![name.to_owned()])?.remove(0);
    if route.destination.is_some() {
        bail!("server {name:?} is remote; it cannot have a service on this host");
    }
    let target = catalog.get(name).expect("a resolved target");
    if matches!(target, Target::Vm { .. })
        || matches!(
            target,
            Target::Local {
                config_dir: None,
                state_dir: None
            }
        )
    {
        catalog.validate_execution(
            std::slice::from_ref(&route),
            &crate::config::Config::load_from(&crate::config::config_path())?,
        )?;
    }
    let transport = target.transport();
    SERVICE_CONTEXT
        .set(ServiceContext {
            route,
            identity: TargetIdentity {
                name: name.to_owned(),
                transport: transport.into(),
            },
        })
        .map_err(|_| anyhow::anyhow!("a service target is already active in this process"))
}

pub(crate) fn effective_vm_context() -> Result<Option<SelectedVmContext>> {
    match selected_vm_context()? {
        Some(selected) => Ok(Some(selected)),
        None if service_context_is_active() => Ok(None),
        None if !crate::vm::in_guest() => Catalog::sole_owned_vm_context(),
        None => Ok(None),
    }
}

pub(crate) fn save_selected_vm(name: &str, config: &crate::config::VmConfig) -> Result<()> {
    let mut catalog = Catalog::load()?;
    let target = catalog
        .servers
        .get_mut(name)
        .with_context(|| format!("selected VM server {name:?} is no longer configured"))?;
    let Target::Vm {
        runtime_name,
        backend,
        config: owned,
    } = target
    else {
        bail!("selected server {name:?} is no longer a managed VM");
    };
    *runtime_name = config.name.clone();
    *backend = config.backend.map(|backend| backend.to_string());
    *owned = Some(Box::new(config.clone()));
    catalog.validate()?;
    catalog.save()
}

fn overlaps(one: &std::path::Path, two: &std::path::Path) -> bool {
    one.starts_with(two) || two.starts_with(one)
}

fn validate_owned_dir(server: &str, field: &str, value: &str) -> Result<PathBuf> {
    let path = crate::config::expand_tilde(value);
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        bail!("server {server:?} {field} must be an absolute path without `..`, got {value:?}");
    }
    Ok(path.components().collect())
}

fn insert_unique_dir<'a>(
    paths: &mut BTreeMap<PathBuf, &'a str>,
    path: PathBuf,
    server: &'a str,
    kind: &str,
) -> Result<()> {
    if let Some(other) = paths.insert(path.clone(), server) {
        bail!(
            "servers {other:?} and {server:?} share {kind} directory {}",
            path.display()
        );
    }
    Ok(())
}

fn insert_unique_port<'a>(
    ports: &mut BTreeMap<u16, (&'a str, &'static str)>,
    port: u16,
    server: &'a str,
    purpose: &'static str,
) -> Result<()> {
    if let Some((other, other_purpose)) = ports.insert(port, (server, purpose)) {
        bail!(
            "server {server:?} {purpose} port {port} conflicts with server {other:?} {other_purpose} port"
        );
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<()> {
    let valid = (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && name
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && !matches!(name, "." | "..");
    if !valid {
        bail!(
            "invalid server name {name:?}; use 1-64 ASCII letters, digits, `.`, `_` or `-`, starting with a letter or digit"
        );
    }
    Ok(())
}

fn validate_runtime_name(server: &str, name: &str) -> Result<()> {
    if name.is_empty()
        || std::path::Path::new(name).is_absolute()
        || std::path::Path::new(name)
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        bail!("server {server:?} has unsafe VM runtime_name {name:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(body: &str) -> Result<Catalog> {
        let _sandbox = crate::config::test_support::sandbox();
        crate::config::write_atomic(&path(), body.as_bytes(), 0o600)?;
        Catalog::load()
    }

    fn owned_vm_with_backend(
        runtime: &str,
        dir: &str,
        ssh_port: u16,
        backend: crate::config::BackendKind,
    ) -> Target {
        let config = crate::config::VmConfig {
            enabled: true,
            name: runtime.into(),
            dir: dir.into(),
            ssh_port,
            backend: Some(backend),
            ..Default::default()
        };
        Target::Vm {
            runtime_name: runtime.into(),
            backend: Some(backend.to_string()),
            config: Some(Box::new(config)),
        }
    }

    fn owned_vm(runtime: &str, dir: &str, ssh_port: u16) -> Target {
        owned_vm_with_backend(
            runtime,
            dir,
            ssh_port,
            crate::config::BackendKind::Firecracker,
        )
    }

    #[test]
    fn zero_one_many_resolution_is_deterministic() {
        let none = Catalog::default();
        assert_eq!(none.resolve(vec![]).unwrap()[0].destination, None);
        assert_eq!(
            none.resolve(vec!["person@host".into()]).unwrap()[0].destination,
            Some("person@host".into())
        );

        let one = load("[servers.ssf-server]\ntransport = \"vm\"\n").unwrap();
        assert_eq!(
            one.resolve(vec![]).unwrap()[0].name.as_deref(),
            Some("ssf-server")
        );

        let many = load(
            "[servers.cloud]\ntransport = \"ssh\"\ndestination = \"cloud.example\"\n\n[servers.ssf-server]\ntransport = \"vm\"\n",
        )
        .unwrap();
        let error = many.resolve(vec![]).unwrap_err().to_string();
        assert!(error.contains("cloud\n  ssf-server"), "{error}");
        assert_eq!(
            many.resolve(vec!["cloud".into()]).unwrap()[0]
                .destination
                .as_deref(),
            Some("cloud.example")
        );
    }

    #[test]
    fn a_catalog_never_treats_an_unknown_name_as_an_ssh_host() {
        let catalog = load("[servers.local]\ntransport = \"local\"\n").unwrap();
        let error = catalog
            .resolve(vec!["typo.example".into()])
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown SSF server"), "{error}");
    }

    #[test]
    fn validation_refuses_unsafe_or_not_yet_isolated_targets() {
        assert!(load("[servers.'../bad']\ntransport = \"local\"\n").is_err());
        assert!(load("[servers.cloud]\ntransport = \"ssh\"\ndestination = \"\"\n").is_err());
        assert!(load("[servers.one]\ntransport = \"local\"\nconfig_dir = \"/tmp/one\"\n[servers.two]\ntransport = \"vm\"\n").is_err());
        assert!(
            load("[servers.one]\ntransport = \"local\"\nconfig_dir = \"/tmp/shared\"\nstate_dir = \"/tmp/one-state\"\n[servers.two]\ntransport = \"local\"\nconfig_dir = \"/tmp/shared/two\"\nstate_dir = \"/tmp/two-state\"\n")
                .unwrap_err()
                .to_string()
                .contains("overlaps")
        );
        let sandbox = crate::config::test_support::sandbox();
        let nested = format!(
            "[servers.one]\ntransport = \"local\"\nconfig_dir = {:?}\nstate_dir = \"/tmp/one-state\"\n",
            sandbox.config_dir().join("nested")
        );
        crate::config::write_atomic(&path(), nested.as_bytes(), 0o600).unwrap();
        assert!(
            Catalog::load()
                .unwrap_err()
                .to_string()
                .contains("overlaps legacy")
        );
    }

    #[test]
    fn owned_vms_may_coexist_only_with_distinct_host_resources() {
        let _sandbox = crate::config::test_support::sandbox();
        let distinct = Catalog {
            servers: BTreeMap::from([
                ("one".into(), owned_vm("one", "/tmp/ssf-vm-one", 2222)),
                ("two".into(), owned_vm("two", "/tmp/ssf-vm-two", 2232)),
            ]),
            exists: true,
        };
        distinct.validate().unwrap();

        for (replacement, expected) in [
            (owned_vm("one", "/tmp/other-vms", 2232), "runtime name"),
            (owned_vm("three", "/tmp/other-vms", 2222), "SSH port"),
            (
                owned_vm("three", "/tmp/other-vms", 2223),
                "Firecracker build",
            ),
            (
                owned_vm("nested", "/tmp/ssf-vm-one/nested", 2232),
                "overlaps",
            ),
        ] {
            let catalog = Catalog {
                servers: BTreeMap::from([
                    ("one".into(), owned_vm("one", "/tmp/ssf-vm-one", 2222)),
                    ("two".into(), replacement),
                ]),
                exists: true,
            };
            let error = catalog.validate().unwrap_err().to_string();
            assert!(error.contains(expected), "{error}");
        }
    }

    #[test]
    fn an_unqualified_process_never_guesses_between_owned_vms() {
        let _sandbox = crate::config::test_support::sandbox();
        let catalog = Catalog {
            servers: BTreeMap::from([
                ("one".into(), owned_vm("one", "/tmp/ssf-vm-one", 2222)),
                ("two".into(), owned_vm("two", "/tmp/ssf-vm-two", 2232)),
            ]),
            exists: true,
        };
        catalog.validate().unwrap();
        catalog.save().unwrap();
        let error = Catalog::sole_owned_vm_context().unwrap_err().to_string();
        assert!(error.contains("needs an explicit server target"), "{error}");
    }

    #[test]
    fn two_lima_targets_derive_distinct_instance_and_disk_identities() {
        let _sandbox = crate::config::test_support::sandbox();
        let one = owned_vm_with_backend(
            "one",
            "/tmp/ssf-lima-one",
            2222,
            crate::config::BackendKind::Lima,
        );
        let two = owned_vm_with_backend(
            "two",
            "/tmp/ssf-lima-two",
            2223,
            crate::config::BackendKind::Lima,
        );
        let catalog = Catalog {
            servers: BTreeMap::from([("one".into(), one.clone()), ("two".into(), two.clone())]),
            exists: true,
        };
        catalog.validate().unwrap();

        let vm = |target: Target| match target {
            Target::Vm {
                config: Some(config),
                ..
            } => crate::vm::Vm::new(&crate::config::Config {
                vm: *config,
                ..Default::default()
            }),
            _ => unreachable!(),
        };
        let one = vm(one);
        let two = vm(two);
        assert_ne!(one.dir, two.dir);
        assert_ne!(one.lima_name(), two.lima_name());
        assert_ne!(one.lima_disk_name(), two.lima_disk_name());
    }

    #[test]
    fn named_local_routes_must_match_the_legacy_mode_they_wrap() {
        let local = load("[servers.local]\ntransport = \"local\"\n").unwrap();
        let local_route = local.resolve(vec![]).unwrap();
        let mut config = crate::config::Config::default();
        local.validate_execution(&local_route, &config).unwrap();
        config.vm.enabled = true;
        assert!(local.validate_execution(&local_route, &config).is_err());

        let vm = load("[servers.ssf-server]\ntransport = \"vm\"\n").unwrap();
        let vm_route = vm.resolve(vec![]).unwrap();
        vm.validate_execution(&vm_route, &config).unwrap();
        config.vm.name = "other".into();
        assert!(vm.validate_execution(&vm_route, &config).is_err());
    }

    #[test]
    fn namespaced_local_targets_have_distinct_contexts_beside_a_vm() {
        let catalog = load(
            "[servers.ssf-server]\ntransport = \"vm\"\n\n[servers.one]\ntransport = \"local\"\nconfig_dir = \"/tmp/ssf-one-config\"\nstate_dir = \"/tmp/ssf-one-state\"\n\n[servers.two]\ntransport = \"local\"\nconfig_dir = \"/tmp/ssf-two-config\"\nstate_dir = \"/tmp/ssf-two-state\"\n",
        )
        .unwrap();
        let route = catalog.resolve(vec!["two".into()]).unwrap().remove(0);
        assert_eq!(
            route.local_context,
            Some(LocalContext {
                config_dir: "/tmp/ssf-two-config".into(),
                state_dir: "/tmp/ssf-two-state".into(),
            })
        );
    }

    #[test]
    fn legacy_vm_migration_is_in_place_idempotent_and_conflict_safe() {
        let _sandbox = crate::config::test_support::sandbox();
        let legacy = crate::config::Config {
            vm: crate::config::VmConfig {
                enabled: true,
                name: "existing".into(),
                dir: "/var/lib/ssf-existing".into(),
                backend: Some(crate::config::BackendKind::Lima),
                ssh_port: 2244,
                ..Default::default()
            },
            ..Default::default()
        };
        legacy.save().unwrap();
        crate::config::write_atomic(
            &path(),
            b"[servers.ssf-server]\ntransport = \"vm\"\nruntime_name = \"existing\"\nbackend = \"lima\"\n",
            0o600,
        )
        .unwrap();

        assert!(Catalog::migrate_legacy_vm("ssf-server").unwrap());
        assert!(
            crate::config::Config::legacy_vm_settings()
                .unwrap()
                .is_none()
        );
        let supervised = crate::config::Config::load().unwrap();
        assert!(supervised.vm.enabled);
        assert_eq!(supervised.vm.name, "existing");
        assert_eq!(supervised.vm.ssh_port, 2244);
        let catalog = Catalog::load().unwrap();
        let route = catalog
            .resolve(vec!["ssf-server".into()])
            .unwrap()
            .remove(0);
        assert_eq!(route.vm_context.as_ref().unwrap().config, legacy.vm);
        assert!(!Catalog::migrate_legacy_vm("ssf-server").unwrap());

        let mut conflicting = legacy.clone();
        conflicting.vm.ssh_port = 2255;
        conflicting.save().unwrap();
        let error = Catalog::migrate_legacy_vm("ssf-server")
            .unwrap_err()
            .to_string();
        assert!(error.contains("conflict"), "{error}");
        assert!(
            crate::config::Config::legacy_vm_settings()
                .unwrap()
                .is_some()
        );
        assert_eq!(
            Catalog::load()
                .unwrap()
                .resolve(vec!["ssf-server".into()])
                .unwrap()
                .remove(0)
                .vm_context
                .unwrap()
                .config
                .ssh_port,
            2244
        );
    }
}
