use super::*;
use crate::config::BackendKind;

fn vm() -> Vm {
    let mut cfg = Config::default();
    cfg.vm.dir = "/v".into();
    cfg.vm.name = "one".into();
    cfg.vm.backend = Some(BackendKind::Lima);
    cfg.vm.vcpus = Some(3);
    cfg.vm.mem_mib = Some(8192);
    cfg.vm.data_gib = Some(40);
    Vm::new(&cfg)
}

mod operations;
mod safety;
mod template;

/// A `Vm` whose `limactl` is a script that answers `list` and `disk
/// list` and records everything it is asked, with both templates left
/// saying `format: true` as a build that died would leave them.
struct Fake {
    vm: Vm,
    instance: Instance,
    dir: PathBuf,
}

/// What the fake's `limactl edit --set` does: what lima's does, or
/// what a lima whose restricted `yq` matched nothing would do -- exit
/// 0 and leave the file exactly as it was.
#[derive(Clone, Copy, PartialEq)]
enum Edit {
    Applies,
    Ignored,
}

/// What the fake's `limactl disk list --json` does. `Fails` is a
/// lima home someone else holds the lock on; `Empty` is a lima that
/// has no such disk.
#[derive(Clone, Copy, PartialEq)]
enum DiskList {
    Answers,
    Empty,
    Fails,
}

/// What the fake's `limactl list --json` does. It is both the
/// liveness question every caller of [`Vm::running_state`] and
/// [`Vm::running_now`] asks and the "is there an instance at all"
/// question [`Vm::lima_survey`] asks, so it has to be able to name
/// the instance, name nothing (lima never had it, or it has been
/// deleted), fail the way a locked lima home fails, and stop
/// returning. A fork that failed under load, a lima home under
/// someone else's lock and a limactl that hangs are all things a
/// laptop does; none of them is the VM having exited.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Listing {
    Answers,
    Empty,
    Fails,
    Hangs,
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Fake {
    fn new(status: &str) -> Self {
        Self::with(status, Edit::Applies, DiskList::Answers)
    }

    fn with(status: &str, edit: Edit, disks: DiskList) -> Self {
        Self::with_all(status, edit, disks, Listing::Answers)
    }

    /// A running instance whose listing behaves like this.
    fn listing(listing: Listing) -> Self {
        Self::with_all("Running", Edit::Applies, DiskList::Answers, listing)
    }

    fn with_all(status: &str, edit: Edit, disks: DiskList, listing: Listing) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ssf-lima-fake-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let inst_dir = dir.join("lima/ssf-one");
        std::fs::create_dir_all(&inst_dir).unwrap();
        std::fs::write(inst_dir.join("ssf-safe-root-v2"), "1").unwrap();
        let log = dir.join("limactl.log");
        let json = format!(
            r#"{{"name":"ssf-one","status":"{status}","dir":"{}","sshLocalPort":2222,"cpus":3,"memory":8589934592}}"#,
            inst_dir.display()
        );
        let limactl = dir.join("limactl");
        let yaml = inst_dir.join("lima.yaml");
        let disk_arm = match disks {
                DiskList::Answers => r#"echo '{"name":"ssf-one","size":21474836480,"dir":"/d","mountPoint":"/mnt/lima-ssf-one"}'"#.to_string(),
                DiskList::Empty => ":".to_string(),
                DiskList::Fails => {
                    r#"echo 'FATAL[0000] failed to lock the lima home' >&2; exit 1"#.to_string()
                }
            };
        // lima's own `--set` rewrites the instance's copy in place.
        let list_arm = match listing {
            Listing::Answers => format!("echo '{json}'"),
            Listing::Empty => ":".to_string(),
            Listing::Fails => {
                r#"echo 'FATAL[0000] failed to lock the lima home' >&2; exit 1"#.to_string()
            }
            // Long enough that no bound this test asks for expires
            // on its own. `exec` so that the fake shell *is* the
            // sleep: what the probe kills is its direct child, and a
            // `sleep` forked under that shell would outlive the test
            // by two minutes, once per run and again under
            // `makepkg`'s check().
            Listing::Hangs => "exec sleep 120".to_string(),
        };
        let edit_arm = match edit {
            Edit::Applies => format!(
                "sed 's/format: true/format: false/' {y} > {y}.new && mv {y}.new {y}",
                y = yaml.display()
            ),
            Edit::Ignored => ":".to_string(),
        };
        std::fs::write(
            &limactl,
            // `shell` fails the way limactl fails against an instance
            // that is not running, so a wait that got that far ends
            // on the instance's state instead of sitting out
            // PROVISION_TIMEOUT.
            format!(
                r#"#!/bin/sh
shift
printf '%s\n' "$*" >> {log}
case "$*" in
  'disk list --json') {disk_arm} ;;
  'list --json') {list_arm} ;;
  edit*--set*) {edit_arm} ;;
  shell*) echo 'instance "ssf-one" is stopped, run `limactl start ssf-one`' >&2; exit 1 ;;
esac
exit 0
"#,
                log = log.display(),
            ),
        )
        .unwrap();
        make_executable(&limactl).unwrap();
        let mut cfg = Config::default();
        cfg.vm.dir = dir.join("vm").to_string_lossy().into_owned();
        cfg.vm.name = "one".into();
        cfg.vm.backend = Some(BackendKind::Lima);
        cfg.vm.limactl = Some(limactl.to_string_lossy().into_owned());
        // `share/` is written on the way into a start, and the seed
        // tree in it holds the guest's own `ssf` binary. On a Linux
        // host that is this process's binary -- 150 MB of debug build
        // copied into the temporary directory on every run of this
        // test. The fake stands in for it: what is being tested here
        // is the order of the steps, not what the seed carries.
        cfg.vm.guest_binary = Some(limactl.to_string_lossy().into_owned());
        std::fs::write(dir.join("ssf-server"), "server").unwrap();
        let mut vm = Vm::new(&cfg);
        // lima's home, where the instance directory the fake made
        // lives and where `_disks/` would be: the tests must not
        // reach the person's own `~/.lima`.
        vm.lima_home = Some(dir.join("lima"));
        std::fs::create_dir_all(&vm.dir).unwrap();
        // What a build that died after `limactl create` leaves: both
        // copies of the template still say `format: true`.
        std::fs::write(vm.template_path(), vm.lima_template(true).unwrap()).unwrap();
        std::fs::write(inst_dir.join("lima.yaml"), vm.lima_template(true).unwrap()).unwrap();
        let instance = parse_instances(&json).pop().expect("one instance");
        Self { vm, instance, dir }
    }

    fn instance_yaml(&self) -> PathBuf {
        PathBuf::from(&self.instance.dir).join("lima.yaml")
    }

    fn commands(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("limactl.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn ran(&self, verb: &str) -> bool {
        self.commands().iter().any(|c| c.starts_with(verb))
    }
}
