//! Client-owned names and routes for independently operated SSF factories.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

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
    Local,
    Vm {
        #[serde(default = "default_runtime_name")]
        runtime_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        backend: Option<String>,
    },
    Ssh {
        destination: String,
    },
}

impl Target {
    pub(crate) fn transport(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Vm { .. } => "vm",
            Self::Ssh { .. } => "ssh",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Route {
    pub name: Option<String>,
    pub destination: Option<String>,
}

fn default_runtime_name() -> String {
    "default".into()
}

pub(crate) fn path() -> PathBuf {
    crate::config::config_dir().join("servers.toml")
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
        let mut managed_here = Vec::new();
        for (name, target) in &self.servers {
            validate_name(name)?;
            match target {
                Target::Local => managed_here.push(name.as_str()),
                Target::Vm {
                    runtime_name,
                    backend,
                } => {
                    validate_runtime_name(name, runtime_name)?;
                    if let Some(backend) = backend
                        && !matches!(backend.as_str(), "firecracker" | "lima")
                    {
                        bail!(
                            "server {name:?} has unknown VM backend {backend:?}; expected `firecracker` or `lima`"
                        );
                    }
                    managed_here.push(name.as_str());
                }
                Target::Ssh { destination } => {
                    if destination.is_empty() || destination.chars().any(char::is_control) {
                        bail!("server {name:?} has an empty or invalid SSH destination");
                    }
                }
            }
        }
        // Phase one deliberately refuses the combination that the later target-
        // context and per-service work will make safe. Treating two names as the
        // current one config/state directory would be worse than no support.
        if managed_here.len() > 1 {
            bail!(
                "this version supports only one locally managed server; found {}",
                managed_here.join(", ")
            );
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

    /// Until target-specific config and state paths land, a named local route
    /// must describe the one factory the legacy config already selects. This
    /// prevents a catalog name from promising isolation that is not implemented.
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
                Target::Local if config.vm.enabled => bail!(
                    "server {name:?} is `local`, but the existing configuration has VM mode enabled"
                ),
                Target::Vm {
                    runtime_name,
                    backend,
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
                Target::Local | Target::Ssh { .. } => {}
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
                Target::Local | Target::Vm { .. } => None,
                Target::Ssh { destination } => Some(destination.clone()),
            },
        })
    }
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
        assert!(
            load("[servers.one]\ntransport = \"local\"\n[servers.two]\ntransport = \"vm\"\n")
                .unwrap_err()
                .to_string()
                .contains("only one locally managed server")
        );
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
}
