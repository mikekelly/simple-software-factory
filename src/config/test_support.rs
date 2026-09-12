use std::cell::RefCell;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

thread_local! {
    /// A stack, so a nested guard puts the outer one back on drop.
    static ACTIVE: RefCell<Vec<Dirs>> = const { RefCell::new(Vec::new()) };
}

/// What the guard on top of the stack points the three directories at.
enum Dirs {
    /// A temporary root with `config`, `state` and `home` under it.
    Sandbox(PathBuf),
    /// The machine's own, for the live tests.
    Machine,
}

/// A temporary config and state directory, in force for the thread that
/// made it until the guard is dropped, and deleted with it. The test
/// harness gives each test its own thread, so this is per-test
/// isolation: two tests running in parallel cannot see each other's
/// `state.json`.
#[must_use = "the sandbox only holds while the guard is alive"]
pub struct Sandbox {
    root: PathBuf,
    /// Not `Send`: dropping the guard on another thread would delete
    /// the directory without taking it off the stack of the thread
    /// that made it, which would go on resolving to a path that is
    /// gone.
    _thread_bound: PhantomData<*const ()>,
}

/// Point `config_dir()` and `state_dir()` at a fresh temporary
/// directory for this thread. Both exist by the time this returns, so a
/// test can lay a fixture down before ssf writes.
pub fn sandbox() -> Sandbox {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "ssf-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    for sub in ["config", "state", "home"] {
        std::fs::create_dir_all(root.join(sub))
            .unwrap_or_else(|e| panic!("creating test sandbox {}: {e}", root.display()));
    }
    ACTIVE.with(|s| s.borrow_mut().push(Dirs::Sandbox(root.clone())));
    Sandbox {
        root,
        _thread_bound: PhantomData,
    }
}

impl Sandbox {
    /// The directory holding both, for a test that wants to put
    /// something beside them.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// What `config::config_dir()` answers while this guard is alive.
    pub fn config_dir(&self) -> PathBuf {
        self.root.join("config")
    }

    /// What `config::state_dir()` answers while this guard is alive.
    pub fn state_dir(&self) -> PathBuf {
        self.root.join("state")
    }

    /// What the daemon takes for `$HOME` while this guard is alive
    /// (`crate::ui`'s Omarchy paths hang off it).
    pub fn home(&self) -> PathBuf {
        self.root.join("home")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        ACTIVE.with(|s| {
            let mut stack = s.borrow_mut();
            if let Some(at) = stack
                .iter()
                .rposition(|d| matches!(d, Dirs::Sandbox(p) if p == &self.root))
            {
                stack.remove(at);
            }
        });
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The machine's own config, state and home directories, for the
/// `#[ignore]`d live tests: they are run by hand against the factory
/// installed here, and `vm_live` cannot seed its guest without
/// resolving the config directory the way the daemon does. The guard
/// itself creates nothing and deletes nothing, but it hands out real
/// paths, so nothing `cargo test` runs on its own may *write* through
/// one — that is #140 again, with no panic left to catch it. The one
/// test below that holds one only compares paths.
#[must_use = "the directories are only the machine's while the guard is alive"]
pub struct TheMachineItself {
    _thread_bound: PhantomData<*const ()>,
}

/// Answer the three directories from the machine itself for as long as
/// the guard is alive. See [`TheMachineItself`].
pub fn the_machine_itself() -> TheMachineItself {
    ACTIVE.with(|s| s.borrow_mut().push(Dirs::Machine));
    TheMachineItself {
        _thread_bound: PhantomData,
    }
}

impl Drop for TheMachineItself {
    fn drop(&mut self) {
        ACTIVE.with(|s| {
            let mut stack = s.borrow_mut();
            if let Some(at) = stack.iter().rposition(|d| matches!(d, Dirs::Machine)) {
                stack.remove(at);
            }
        });
    }
}

/// The sandbox's stand-in for `$HOME`, for the paths outside ssf's own
/// directories that the daemon writes to (`crate::ui`, which installs
/// and removes the Omarchy widget under `~/.config/omarchy`).
pub(crate) fn home() -> PathBuf {
    require("home")
}

/// The sandbox's `which` subdirectory for the calling thread, or a
/// panic naming what the test has to do about it.
pub(super) fn require(which: &str) -> PathBuf {
    ACTIVE.with(|s| match s.borrow().last() {
        Some(Dirs::Sandbox(root)) => root.join(which),
        Some(Dirs::Machine) => match which {
            "config" => super::real_config_dir(),
            "state" => super::real_state_dir(),
            "home" => dirs::home_dir().unwrap_or_else(|| PathBuf::from("~")),
            _ => panic!("no machine directory for {which:?}"),
        },
        None => panic!(
            "this test reached the real {which} directory. Tests must \
                 not write outside a temporary directory of their own: \
                 hold a \
                 `let _sandbox = crate::config::test_support::sandbox();` \
                 guard for as long as the test needs one (#140). A test \
                 that holds one and still sees this is resolving the \
                 directory on some other thread than its own, which the \
                 guard does not reach."
        ),
    })
}
