use super::*;
use crate::config::BackendKind;

fn spec() -> Spec<'static> {
    Spec {
        instance: "ssf-one",
        image: DEFAULT_IMAGE,
        pool: "default",
        volume: "ssf-one",
        share: "/v/one/share",
        ssh_port: 2222,
        vcpus: 3,
        mem_mib: 8192,
        owner: "1000",
    }
}

#[test]
fn the_container_is_unprivileged_nested_and_sized_from_vm() {
    let args = init_args(&spec());
    assert_eq!(&args[..3], ["init", "images:ubuntu/24.04", "ssf-one"]);
    let keys: Vec<&str> = args
        .windows(2)
        .filter(|w| w[0] == "-c")
        .map(|w| w[1].as_str())
        .collect();
    assert_eq!(
        keys,
        [
            "security.nesting=true",
            "security.syscalls.intercept.mknod=true",
            "security.syscalls.intercept.setxattr=true",
            "user.ssf.owner=1000",
            "limits.cpu=3",
            "limits.memory=8192MiB",
        ]
    );
    // Never privileged: that would make container root the host's root.
    assert!(!args.iter().any(|a| a.contains("privileged")), "{args:?}");
}

#[test]
fn the_devices_are_the_share_the_data_volume_and_the_ssh_proxy() {
    let devs = device_args(&spec());
    assert_eq!(
        devs[0],
        [
            "config",
            "device",
            "add",
            "ssf-one",
            SHARE_DEVICE,
            "disk",
            "source=/v/one/share",
            "path=/mnt/ssf",
            "readonly=true",
            "shift=true"
        ]
    );
    assert_eq!(
        devs[1][4..],
        [
            DATA_DEVICE,
            "disk",
            "pool=default",
            "source=ssf-one",
            "path=/var/lib/ssf"
        ]
    );
    // The proxy listens on the host's loopback only.
    assert_eq!(
        devs[2][4..],
        [
            SSH_DEVICE,
            "proxy",
            "listen=tcp:127.0.0.1:2222",
            "connect=tcp:127.0.0.1:22"
        ]
    );
}

#[test]
fn the_boot_script_runs_the_shared_guest_boot_script() {
    let s = boot_script();
    assert!(s.ends_with("exec bash /mnt/ssf/guest/lima-boot.sh"), "{s}");
    // The log is emptied only for a provisioning attempt, so a later
    // start keeps the first boot's log to read.
    let truncate = s.find(": > /var/log/ssf-provision.log").unwrap();
    assert!(s.find("if [ ! -f /etc/ssf-image-built ]").unwrap() < truncate);
}

#[test]
fn incus_list_and_volume_json_are_read() {
    let list = r#"[{"name":"ssf-one","status":"Running","type":"container","config":{"limits.cpu":"3"}},
                  {"name":"ssf-one-other","status":"Stopped"}]"#;
    let inst = parse_instances(list).unwrap();
    assert_eq!(inst.len(), 2);
    assert!(inst[0].is_running());
    assert!(!inst[1].is_running());
    assert!(parse_instances("[]").unwrap().is_empty());
    assert!(parse_instances("not json").is_err());

    let vols = r#"[{"name":"ssf-one","type":"custom","config":{"size":"40GiB"}},
                  {"name":"ssf-one","type":"container","config":{}},
                  {"name":"ssf-two","type":"custom","config":{}}]"#;
    let vols = parse_volumes(vols).unwrap();
    assert_eq!(vols[0].size_bytes(), Some(40 << 30));
    assert_eq!(vols[1].kind, "container");
    assert_eq!(vols[2].size_bytes(), None);
}

#[test]
fn incus_sizes_are_read_in_both_unit_systems() {
    assert_eq!(parse_size("20GiB"), Some(20 << 30));
    assert_eq!(parse_size("512MiB"), Some(512 << 20));
    assert_eq!(parse_size("10GB"), Some(10_000_000_000));
    assert_eq!(parse_size("1073741824"), Some(1 << 30));
    assert_eq!(parse_size("1.5GiB"), Some(3 << 29));
    assert_eq!(parse_size("lots"), None);
    assert_eq!(parse_size("5XB"), None);
}

#[test]
fn names_must_make_a_container_name() {
    assert!(check_name("default").is_ok());
    assert!(check_name("work-2").is_ok());
    assert!(check_name("a_b").is_err());
    assert!(check_name("a.b").is_err());
    assert!(check_name("trailing-").is_err());
    assert!(check_name(&"x".repeat(60)).is_err());
    assert!(check_name(&"x".repeat(59)).is_ok());
}

#[test]
fn the_daemon_socket_follows_incus_own_environment() {
    assert_eq!(
        socket_path_from(None, None),
        PathBuf::from("/var/lib/incus/unix.socket")
    );
    assert_eq!(
        socket_path_from(None, Some("/srv/incus")),
        PathBuf::from("/srv/incus/unix.socket")
    );
    assert_eq!(
        socket_path_from(Some("/run/i.sock"), Some("/srv/incus")),
        PathBuf::from("/run/i.sock")
    );
    assert_eq!(
        socket_path_from(Some(" "), None),
        PathBuf::from("/var/lib/incus/unix.socket")
    );
}

#[test]
fn incus_shares_the_host_kernel_and_the_others_do_not() {
    assert!(BackendKind::Incus.shares_host_kernel());
    assert!(!BackendKind::Lima.shares_host_kernel());
    assert!(!BackendKind::Firecracker.shares_host_kernel());
}

#[test]
fn the_seed_env_tells_the_seed_to_take_the_incus_volume() {
    use super::super::lima::seed_env;
    assert!(seed_env("one", BackendKind::Incus).contains("SSF_VM_BACKEND=incus\n"));
    assert!(!seed_env("one", BackendKind::Lima).contains("SSF_VM_BACKEND"));
    let read = |f: &str| {
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(f)).unwrap()
    };
    assert!(read("vm/guest/seed-lima.sh").contains(r#"[ "${SSF_VM_BACKEND:-lima}" = incus ]"#));
    assert!(read("vm/guest/lima-boot.sh").contains("SSF_VM_BACKEND=${SSF_VM_BACKEND:-lima}"));
    assert!(read("vm/guest/provision.sh").contains("lima|incus)"));
}

#[test]
fn only_this_users_container_or_volume_is_touched() {
    assert!(check_owner("container", "ssf-one", Some("1000"), "1000").is_ok());
    let e = check_owner("container", "ssf-one", Some("1001"), "1000")
        .unwrap_err()
        .to_string();
    assert!(e.contains("uid 1001") && e.contains("ssf-one"), "{e}");
    let e = check_owner("volume", "ssf-one", None, "1000")
        .unwrap_err()
        .to_string();
    assert!(e.contains(OWNER_KEY) && e.contains("ssf-one"), "{e}");
}

#[test]
fn the_owner_and_the_data_pool_come_from_the_listing() {
    let list = r#"[{"name":"ssf-one","status":"Stopped",
        "config":{"user.ssf.owner":"1000"},
        "devices":{"ssf-data":{"type":"disk","pool":"fast","source":"ssf-one"}}},
        {"name":"ssf-two","status":"Running"}]"#;
    let i = parse_instances(list).unwrap();
    assert_eq!(i[0].owner(), Some("1000"));
    assert_eq!(i[0].data_pool(), Some("fast"));
    assert_eq!(i[1].owner(), None);
    assert_eq!(i[1].data_pool(), None);
    let v =
        parse_volumes(r#"[{"name":"ssf-one","type":"custom","config":{"user.ssf.owner":"7"}}]"#)
            .unwrap();
    assert_eq!(v[0].owner(), Some("7"));
    let p = parse_pools(r#"[{"name":"default","driver":"dir"},{"name":"fast"}]"#).unwrap();
    assert_eq!(p.len(), 2);
}
