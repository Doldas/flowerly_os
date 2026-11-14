use super::esp::{Esp, MemEsp};
use super::*;
use std::io::Write;

const A_UUID: &str = "11111111-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const B_UUID: &str = "22222222-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const ROOT_MIB: u64 = 3;

fn sha(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn pattern(seed: u8, len: usize) -> Vec<u8> {
    let mut data: Vec<u8> = (0..len)
        .map(|i| (i as u8).wrapping_mul(seed).wrapping_add(seed))
        .collect();
    data[1080] = 0x53;
    data[1081] = 0xEF;
    data
}

struct Fixture {
    dir: tempfile::TempDir,
    disk: std::path::PathBuf,
    manifest: std::path::PathBuf,
    candidate: std::path::PathBuf,
    candidate_sha: String,
    kernel: std::path::PathBuf,
    initramfs: std::path::PathBuf,
    esp: MemEsp,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mib = MIB as usize;
    let root = (ROOT_MIB as usize) * mib;
    let layout = [
        (
            "esp",
            "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee",
            1usize,
            1usize,
        ),
        ("root-a", A_UUID, 2, ROOT_MIB as usize),
        ("root-b", B_UUID, 2 + ROOT_MIB as usize, ROOT_MIB as usize),
        (
            "data",
            "dddddddd-dddd-4ddd-8ddd-dddddddddddd",
            2 + 2 * ROOT_MIB as usize,
            1,
        ),
    ];
    let total = (2 + 2 * ROOT_MIB as usize + 1) * mib;
    let mut image = vec![0u8; total];
    let table: Vec<(&str, &str, u64, u64)> = layout
        .iter()
        .map(|(n, u, off, len)| (*n, *u, (*off * mib / 512) as u64, (*len * mib / 512) as u64))
        .collect();
    let header = gpt::build_for_test(&table, total as u64);
    image[..header.len()].copy_from_slice(&header);
    let a_bytes = pattern(7, root);
    image[2 * mib..2 * mib + root].copy_from_slice(&a_bytes);
    let data_off = (2 + 2 * ROOT_MIB as usize) * mib;
    image[data_off..data_off + 4096].fill(0xD5);
    let disk = dir.path().join("disk.img");
    std::fs::write(&disk, &image).unwrap();
    let part_json: Vec<Value> = layout
        .iter()
        .map(|(n, u, off, len)| {
            let slice = &image[off * mib..(off + len) * mib];
            json!({
                "name": n, "partuuid": u, "offset_bytes": off * mib, "size_bytes": len * mib,
                "sha256": sha(slice),
                "filesystem": match *n { "esp" => "vfat", "root-b" => "empty", _ => "ext4" },
            })
        })
        .collect();
    let manifest = dir.path().join("disk.manifest.json");
    std::fs::write(
        &manifest,
        serde_json::to_vec_pretty(&json!({
            "schema": SCHEMA,
            "builder_commit": "0".repeat(40),
            "disk": {"size_bytes": total, "sha256": sha(&image), "guid": "x"},
            "esp_files": {},
            "partitions": part_json,
        }))
        .unwrap(),
    )
    .unwrap();
    let candidate_bytes = pattern(13, root);
    let candidate = dir.path().join("root-b.ext4");
    std::fs::write(&candidate, &candidate_bytes).unwrap();
    let kernel = dir.path().join("vmlinuz-b");
    std::fs::write(&kernel, b"kernel-b").unwrap();
    let initramfs = dir.path().join("initrd-b");
    std::fs::write(&initramfs, b"initramfs-b").unwrap();
    let mut esp = MemEsp::default();
    esp.dirs.insert("/flowerly".into());
    esp.dirs.insert("/flowerly/a".into());
    esp.files.insert(
        "/loader/loader.conf".into(),
        format!("{LOADER_DEFAULT}\ntimeout 0\n").into_bytes(),
    );
    esp.files.insert(
        "/loader/entries/flowerly-a.conf".into(),
        format!("title Flowerly OS slot A\nlinux /flowerly/a/vmlinuz\ninitrd /flowerly/a/initramfs.img\noptions root=PARTUUID={A_UUID} ro console=ttyS0\n").into_bytes(),
    );
    Fixture {
        dir,
        disk,
        manifest,
        candidate_sha: sha(&candidate_bytes),
        candidate,
        kernel,
        initramfs,
        esp,
    }
}

impl Fixture {
    fn open(&self) -> (Image, Disk) {
        open_bound(&self.disk, &self.manifest, true).unwrap()
    }

    fn slices(&self) -> Vec<String> {
        let (image, disk) = self.open();
        disk.parts
            .iter()
            .map(|p| image.hash(p.offset, p.size).unwrap())
            .collect()
    }

    fn request<'a>(&'a self, target: Slot, sha_value: &'a str) -> StageRequest<'a> {
        StageRequest {
            manifest_path: &self.manifest,
            active: Slot::A,
            target,
            candidate: &self.candidate,
            expected_sha: sha_value,
        }
    }

    fn stage(&self, faults: &Faults) -> Result<Value> {
        let (image, mut disk) = self.open();
        stage(
            &image,
            &mut disk,
            &self.esp,
            &self.request(Slot::B, &self.candidate_sha),
            faults,
        )
    }

    fn arm(&mut self, extra: &[String], faults: &Faults) -> Result<Value> {
        let (image, disk) = self.open();
        let request = ArmRequest {
            kernel: &self.kernel,
            initramfs: &self.initramfs,
            tries: 3,
            extra_options: extra,
        };
        arm(
            &image,
            &disk,
            Slot::A,
            Slot::B,
            &mut self.esp,
            &request,
            faults,
        )
    }

    fn status(&self, witness: Option<&Path>) -> Value {
        let (image, disk) = self.open();
        status(&image, &disk, &self.esp, witness, true).unwrap()
    }

    fn stage_and_arm(&mut self) {
        self.stage(&Faults::none()).unwrap();
        self.arm(&[], &Faults::none()).unwrap();
    }

    fn witness(&self, slots: &[&str], truncate: bool) -> std::path::PathBuf {
        let tag = slots.join("-") + if truncate { "-torn" } else { "" };
        let path = self.dir.path().join(format!("witness-{tag}.jsonl"));
        let mut file = std::fs::File::create(&path).unwrap();
        for (i, slot) in slots.iter().enumerate() {
            writeln!(
                file,
                "{}",
                json!({"sequence": i + 1, "slot": slot, "boot_id": format!("boot-{i}")})
            )
            .unwrap();
        }
        if truncate {
            write!(file, "{{\"sequence\": 99, \"slot\": \"root-b\"").unwrap();
        }
        path
    }
}

fn err(result: Result<Value>) -> String {
    format!("{:#}", result.unwrap_err())
}

#[test]
fn entry_names_follow_systemd_boot_counting() {
    let e = parse_entry("flowerly-b+3.conf").unwrap();
    assert_eq!(
        (e.slot, e.state, e.tries_left, e.tries_done),
        (Slot::B, EntryState::Counting, Some(3), 0)
    );
    let e = parse_entry("flowerly-b+2-1.conf").unwrap();
    assert_eq!((e.tries_left, e.tries_done), (Some(2), 1));
    assert_eq!(
        parse_entry("flowerly-b+0-3.conf").unwrap().state,
        EntryState::Bad
    );
    assert_eq!(
        parse_entry("flowerly-a.conf").unwrap().state,
        EntryState::Good
    );
    for bad in [
        "flowerly-c.conf",
        "flowerly-b+.conf",
        "flowerly-b+x.conf",
        "flowerly-b+1-.conf",
        "flowerly-b+3.conf.tmp",
        "other-a.conf",
        "flowerly-a",
        "flowerly-b+1234.conf",
    ] {
        assert!(parse_entry(bad).is_none(), "{bad}");
    }
}

#[test]
fn derive_entry_swaps_only_the_root_and_validates_extras() {
    let active = format!("title x\noptions console=ttyS0 root=PARTUUID={A_UUID} ro\n");
    let text = derive_entry(
        &active,
        A_UUID,
        Slot::B,
        B_UUID,
        &["flowerly.test.fail_health=1".into()],
    )
    .unwrap();
    assert!(text.contains(&format!(
        "options console=ttyS0 root=PARTUUID={B_UUID} ro flowerly.test.fail_health=1\n"
    )));
    assert!(text.contains("linux /flowerly/b/vmlinuz\ninitrd /flowerly/b/initramfs.img\n"));
    assert!(derive_entry(&active, B_UUID, Slot::B, B_UUID, &[]).is_err());
    assert!(derive_entry("title x\n", A_UUID, Slot::B, B_UUID, &[]).is_err());
    assert!(derive_entry(
        &format!("options root=PARTUUID={A_UUID} root=/dev/x\n"),
        A_UUID,
        Slot::B,
        B_UUID,
        &[]
    )
    .is_err());
    for bad in ["", "a b", "x;y", "$(id)", "a\nb"] {
        assert!(
            derive_entry(&active, A_UUID, Slot::B, B_UUID, &[bad.into()]).is_err(),
            "{bad:?}"
        );
    }
}

#[test]
fn manifest_and_gpt_must_agree() {
    let f = fixture();
    let (_, disk) = f.open();
    assert_eq!(disk.active_slot(&A_UUID.to_uppercase()).unwrap(), Slot::A);
    assert!(disk
        .active_slot("00000000-0000-4000-8000-000000000000")
        .is_err());
    let mut text: Value = serde_json::from_slice(&std::fs::read(&f.manifest).unwrap()).unwrap();
    text["partitions"][2]["partuuid"] = json!("33333333-cccc-4ccc-8ccc-cccccccccccc");
    std::fs::write(&f.manifest, serde_json::to_vec(&text).unwrap()).unwrap();
    assert!(open_bound(&f.disk, &f.manifest, false)
        .err()
        .unwrap()
        .to_string()
        .contains("GPT differs"));
    for edit in [
        |v: &mut Value| v["schema"] = json!("other"),
        |v: &mut Value| v["partitions"].as_array_mut().unwrap().swap(1, 2),
        |v: &mut Value| v["partitions"][1]["sha256"] = json!("zz"),
        |v: &mut Value| v["partitions"][1]["offset_bytes"] = json!(1),
        |v: &mut Value| v["partitions"][3]["partuuid"] = v["partitions"][1]["partuuid"].clone(),
        |v: &mut Value| v["disk"]["size_bytes"] = json!(1),
    ] {
        let mut broken: Value =
            serde_json::from_slice(&std::fs::read(&f.manifest).unwrap()).unwrap();
        broken["partitions"][2]["partuuid"] = json!(B_UUID);
        edit(&mut broken);
        assert!(Disk::parse(&serde_json::to_vec(&broken).unwrap()).is_err());
    }
}

#[test]
fn refuses_symlinks_and_oversized_manifests() {
    let f = fixture();
    let link = f.dir.path().join("link.img");
    std::os::unix::fs::symlink(&f.disk, &link).unwrap();
    assert!(Image::open(&link, false).is_err());
    let big = f.dir.path().join("big.json");
    std::fs::write(&big, vec![b' '; (MIB + 1) as usize]).unwrap();
    assert!(Disk::load(&big).is_err());
}

#[test]
fn stage_writes_only_the_target_slot_and_updates_the_manifest() {
    let f = fixture();
    let before = f.slices();
    let report = f.stage(&Faults::none()).unwrap();
    assert_eq!(report["status"], "STAGED_NOT_ARMED");
    let after = f.slices();
    for index in [0, 1, 3] {
        assert_eq!(
            before[index], after[index],
            "partition {index} must be untouched"
        );
    }
    assert_eq!(after[2], f.candidate_sha);
    let (image, disk) = f.open();
    assert_eq!(disk.slot(Slot::B).sha256, f.candidate_sha);
    assert_eq!(disk.slot(Slot::B).filesystem, "ext4");
    assert_eq!(report["disk_sha256"], image.hash(0, image.size).unwrap());
    let reread: Value = serde_json::from_slice(&std::fs::read(&f.manifest).unwrap()).unwrap();
    assert_eq!(
        reread["builder_commit"],
        "0".repeat(40),
        "unknown fields survive"
    );
}

#[test]
fn stage_refusals_change_nothing() {
    let mut f = fixture();
    let before = f.slices();
    let manifest_before = std::fs::read(&f.manifest).unwrap();
    let (image, mut disk) = f.open();
    let go = |disk: &mut Disk, esp: &MemEsp, req: StageRequest| {
        stage(&image, disk, esp, &req, &Faults::none())
    };
    let ok = f.candidate_sha.clone();
    assert!(err(go(&mut disk, &f.esp, f.request(Slot::A, &ok))).contains("active slot"));
    assert!(
        err(go(&mut disk, &f.esp, f.request(Slot::B, &"0".repeat(64)))).contains("digest mismatch")
    );
    assert!(err(go(&mut disk, &f.esp, f.request(Slot::B, "ABC"))).contains("lowercase hex"));
    let short = f.dir.path().join("short.ext4");
    std::fs::write(&short, vec![0u8; MIB as usize]).unwrap();
    let short_sha = sha(&vec![0u8; MIB as usize]);
    let mut req = f.request(Slot::B, &short_sha);
    req.candidate = &short;
    assert!(err(go(&mut disk, &f.esp, req)).contains("exact root slot size"));
    let blank = f.dir.path().join("blank.img");
    std::fs::write(&blank, vec![0u8; (ROOT_MIB * MIB) as usize]).unwrap();
    let blank_sha = sha(&vec![0u8; (ROOT_MIB * MIB) as usize]);
    let mut req = f.request(Slot::B, &blank_sha);
    req.candidate = &blank;
    assert!(err(go(&mut disk, &f.esp, req)).contains("not an ext4"));
    let link = f.dir.path().join("cand-link");
    std::os::unix::fs::symlink(&f.candidate, &link).unwrap();
    let mut req = f.request(Slot::B, &ok);
    req.candidate = &link;
    assert!(go(&mut disk, &f.esp, req).is_err(), "symlinked candidate");
    drop(image);
    f.esp
        .files
        .insert("/loader/entries/flowerly-b+3.conf".into(), b"x".to_vec());
    assert!(err(f.stage(&Faults::none())).contains("has a boot entry"));
    f.esp.files.remove("/loader/entries/flowerly-b+3.conf");
    f.esp.files.insert(
        "/loader/entries/flowerly-b+3.conf.tmp".into(),
        b"x".to_vec(),
    );
    assert!(err(f.stage(&Faults::none())).contains("has a boot entry"));
    f.esp.files.remove("/loader/entries/flowerly-b+3.conf.tmp");
    assert_eq!(f.slices(), before);
    assert_eq!(std::fs::read(&f.manifest).unwrap(), manifest_before);
    // The active root is verified too: a modified A blocks staging.
    let file = OpenOptions::new().write(true).open(&f.disk).unwrap();
    file.write_all_at(b"tamper", 2 * MIB + 4096).unwrap();
    assert!(err(f.stage(&Faults::none())).contains("active root"));
}

#[test]
fn stage_interrupted_anywhere_never_touches_other_partitions_and_can_be_rerun() {
    let points = [
        "stage:chunk:1",
        "stage:chunk:2",
        "stage:chunk:3",
        "stage:before-readback",
        "stage:before-manifest",
        "stage:manifest-temp",
        "stage:manifest-renamed",
    ];
    for point in points {
        let mut f = fixture();
        let before = f.slices();
        let manifest_before = std::fs::read(&f.manifest).unwrap();
        assert!(
            err(f.stage(&Faults::error_at(point))).contains(point),
            "{point}"
        );
        let after = f.slices();
        for index in [0, 1, 3] {
            assert_eq!(before[index], after[index], "{point}: partition {index}");
        }
        let (_, disk) = f.open();
        let completed = point == "stage:manifest-renamed";
        if completed {
            assert_eq!(disk.slot(Slot::B).sha256, f.candidate_sha);
        } else {
            // The old manifest is intact, so the half-written slot can never
            // be mistaken for a staged one.
            assert_eq!(
                std::fs::read(&f.manifest).unwrap(),
                manifest_before,
                "{point}"
            );
            assert_eq!(disk.slot(Slot::B).filesystem, "empty");
            assert!(
                err(f.arm(&[], &Faults::none())).contains("has not been staged"),
                "{point}"
            );
            let state = f.status(None);
            assert_eq!(
                state["slots"][1]["image_state"],
                if point.starts_with("stage:chunk")
                    || point == "stage:before-readback"
                    || point == "stage:before-manifest"
                    || point == "stage:manifest-temp"
                {
                    if after[2] == before[2] {
                        "MATCHES_MANIFEST"
                    } else {
                        "MODIFIED"
                    }
                } else {
                    "MODIFIED"
                }
            );
        }
        // Recovery needs no manual repair: staging again completes.
        let again = f.stage(&Faults::none());
        if completed {
            assert!(again.is_ok());
        } else {
            assert_eq!(again.unwrap()["status"], "STAGED_NOT_ARMED", "{point}");
        }
        assert_eq!(f.slices()[2], f.candidate_sha);
    }
}

#[test]
fn write_guard_rejects_ranges_outside_the_partition() {
    let f = fixture();
    let (image, disk) = f.open();
    let part = disk.slot(Slot::B);
    assert!(image.write_slot(part, part.size - 1, &[1, 2]).is_err());
    assert!(image.write_slot(part, part.size, &[1]).is_err());
    assert!(image.write_slot(part, 0, &[1]).is_ok());
    drop(image);
    assert_eq!(
        f.slices()[3],
        sha(&{
            let mut data = vec![0u8; MIB as usize];
            data[..4096].fill(0xD5);
            data
        })
    );
}

#[test]
fn arm_publishes_a_pending_entry_and_never_a_success() {
    let mut f = fixture();
    f.stage(&Faults::none()).unwrap();
    let report = f
        .arm(&["flowerly.test.fail_health=1".into()], &Faults::none())
        .unwrap();
    assert_eq!(report["status"], "ARMED_PENDING");
    assert_eq!(report["boot_entry"], "flowerly-b+3.conf");
    let entry =
        String::from_utf8(f.esp.read("/loader/entries/flowerly-b+3.conf").unwrap()).unwrap();
    assert!(entry.contains(&format!("root=PARTUUID={B_UUID}")) && entry.contains("fail_health=1"));
    assert_eq!(f.esp.read("/flowerly/b/vmlinuz").unwrap(), b"kernel-b");
    assert_eq!(
        f.esp.read("/flowerly/b/initramfs.img").unwrap(),
        b"initramfs-b"
    );
    assert!(f.esp.files.contains_key("/loader/entries/flowerly-a.conf"));
    assert!(!f.esp.files.keys().any(|k| k.ends_with(".tmp")));
    assert_eq!(f.status(None)["verdict"], "PENDING");
}

#[test]
fn arm_refusals_leave_the_esp_alone() {
    let mut f = fixture();
    assert!(err(f.arm(&[], &Faults::none())).contains("has not been staged"));
    f.stage(&Faults::none()).unwrap();
    let before = f.esp.clone();
    {
        let (image, disk) = f.open();
        let request = ArmRequest {
            kernel: &f.kernel,
            initramfs: &f.initramfs,
            tries: 3,
            extra_options: &[],
        };
        let mut esp = f.esp.clone();
        assert!(err(arm(
            &image,
            &disk,
            Slot::A,
            Slot::A,
            &mut esp,
            &request,
            &Faults::none()
        ))
        .contains("active slot"));
        for tries in [0, 10] {
            let request = ArmRequest {
                tries,
                ..ArmRequest {
                    kernel: &f.kernel,
                    initramfs: &f.initramfs,
                    tries: 3,
                    extra_options: &[],
                }
            };
            assert!(arm(
                &image,
                &disk,
                Slot::A,
                Slot::B,
                &mut esp,
                &request,
                &Faults::none()
            )
            .is_err());
        }
    }
    assert!(err(f.arm(&["x y".into()], &Faults::none())).contains("unsafe"));
    let loader = f.esp.files["/loader/loader.conf"].clone();
    f.esp.files.insert(
        "/loader/loader.conf".into(),
        b"default flowerly-b*\n".to_vec(),
    );
    assert!(err(f.arm(&[], &Faults::none())).contains("loader.conf"));
    f.esp.files.insert("/loader/loader.conf".into(), loader);
    f.esp.files.remove("/loader/entries/flowerly-a.conf");
    assert!(err(f.arm(&[], &Faults::none())).contains("nothing to fall back to"));
    f.esp.files.insert(
        "/loader/entries/flowerly-a.conf".into(),
        before.files["/loader/entries/flowerly-a.conf"].clone(),
    );
    assert_eq!(f.esp.files, before.files, "refusals left the ESP unchanged");
    // A staged slot that was changed afterwards is refused.
    let file = OpenOptions::new().write(true).open(&f.disk).unwrap();
    file.write_all_at(b"flip", 5 * MIB + 100).unwrap();
    assert!(err(f.arm(&[], &Faults::none())).contains("staged root"));
}

#[test]
fn arm_cannot_run_twice_and_interrupted_arms_recover_by_rerunning() {
    for (point, entry_published) in [
        ("arm:after-payload", false),
        ("arm:after-tmp-entry", false),
        ("arm:after-rename", true),
    ] {
        let mut f = fixture();
        f.stage(&Faults::none()).unwrap();
        assert!(err(f.arm(&[], &Faults::error_at(point))).contains(point));
        let state = f.status(None);
        if entry_published {
            assert_eq!(state["verdict"], "PENDING", "{point}");
            assert!(err(f.arm(&[], &Faults::none())).contains("already has a boot entry"));
        } else {
            // No boot entry for B exists, so the loader can only pick A.
            assert_eq!(state["verdict"], "NO_CANDIDATE", "{point}");
            assert!(state["entries"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| e["slot"] == "root-a"));
            let strays = state["stray_temp_entries"].as_array().unwrap().len();
            assert_eq!(
                strays,
                usize::from(point == "arm:after-tmp-entry"),
                "{point}"
            );
            let report = f.arm(&[], &Faults::none()).unwrap();
            assert_eq!(report["status"], "ARMED_PENDING", "{point}");
            assert!(!f.esp.files.keys().any(|k| k.ends_with(".tmp")));
            assert_eq!(f.status(None)["verdict"], "PENDING");
        }
        // Whatever happened, A's entry and the data/active partitions are intact.
        assert!(f.esp.files.contains_key("/loader/entries/flowerly-a.conf"));
    }
}

#[test]
fn rollback_removes_the_other_slot_entries_and_keeps_a_bootable_one() {
    let mut f = fixture();
    f.stage_and_arm();
    f.esp.files.insert(
        "/loader/entries/flowerly-b+3.conf.tmp".into(),
        b"x".to_vec(),
    );
    let report = {
        let (image, disk) = f.open();
        rollback(&image, &disk, Slot::A, &mut f.esp, false, &Faults::none()).unwrap()
    };
    assert_eq!(report["status"], "ROLLED_BACK");
    assert_eq!(report["removed_entries"].as_array().unwrap().len(), 2);
    assert_eq!(f.status(None)["verdict"], "NO_CANDIDATE");
    assert!(
        f.esp.files.contains_key("/flowerly/b/vmlinuz"),
        "payload is kept"
    );
    let (image, disk) = f.open();
    assert!(err(rollback(
        &image,
        &disk,
        Slot::A,
        &mut f.esp,
        false,
        &Faults::none()
    ))
    .contains("nothing to roll back"));
    f.esp.files.remove("/loader/entries/flowerly-a.conf");
    assert!(err(rollback(
        &image,
        &disk,
        Slot::A,
        &mut f.esp,
        false,
        &Faults::none()
    ))
    .contains("no boot entry"));
}

#[test]
fn rollback_refuses_a_damaged_target_and_survives_interruption() {
    let mut f = fixture();
    f.stage_and_arm();
    let point = "rollback:after-delete:1";
    {
        let mut esp = f.esp.clone();
        let (image, disk) = f.open();
        assert!(err(rollback(
            &image,
            &disk,
            Slot::A,
            &mut esp,
            false,
            &Faults::error_at(point)
        ))
        .contains(point));
        assert!(
            esp.files.contains_key("/loader/entries/flowerly-a.conf"),
            "A stays bootable"
        );
        assert!(
            rollback(&image, &disk, Slot::A, &mut esp, false, &Faults::none()).is_err(),
            "already complete"
        );
    }
    let file = OpenOptions::new().write(true).open(&f.disk).unwrap();
    file.write_all_at(b"rot", 2 * MIB + 8192).unwrap();
    let (image, disk) = f.open();
    assert!(err(rollback(
        &image,
        &disk,
        Slot::A,
        &mut f.esp,
        false,
        &Faults::none()
    ))
    .contains("rollback target"));
    assert!(
        f.esp
            .files
            .contains_key("/loader/entries/flowerly-b+3.conf"),
        "nothing was deleted"
    );
}

#[test]
fn status_never_reports_success_from_entries_alone() {
    let mut f = fixture();
    assert_eq!(f.status(None)["verdict"], "NO_CANDIDATE");
    f.stage_and_arm();
    let rename = |f: &mut Fixture, from: &str, to: &str| {
        let data = f
            .esp
            .files
            .remove(&format!("/loader/entries/{from}"))
            .unwrap();
        f.esp.files.insert(format!("/loader/entries/{to}"), data);
    };
    let verdict = |f: &Fixture, witness: Option<&Path>| {
        f.status(witness)["verdict"].as_str().unwrap().to_owned()
    };
    // Counting: pending whatever the ledger says (not blessed yet).
    let w_b = f.witness(&["root-a", "root-b"], false);
    assert_eq!(verdict(&f, Some(&w_b)), "PENDING");
    // Exhausted entry: falls back only when the witness shows A.
    rename(&mut f, "flowerly-b+3.conf", "flowerly-b+0-3.conf");
    let w_a = f.witness(&["root-a", "root-b", "root-a"], false);
    assert_eq!(verdict(&f, Some(&w_a)), "FELL_BACK");
    assert_eq!(verdict(&f, None), "FELL_BACK_UNCONFIRMED");
    assert_eq!(verdict(&f, Some(&w_b)), "FAILED_UNRESOLVED");
    // A blessed entry is ACTIVE only with a witness.
    rename(&mut f, "flowerly-b+0-3.conf", "flowerly-b.conf");
    assert_eq!(verdict(&f, None), "ACTIVE_UNCONFIRMED");
    assert_eq!(verdict(&f, Some(&w_b)), "ACTIVE");
    assert_eq!(f.status(Some(&w_b))["witness_slot"], "root-b");
    // A health record after the witness does not hide the witnessed slot.
    let healthy = f.dir.path().join("witness-healthy.jsonl");
    std::fs::write(
        &healthy,
        format!(
            "{}\n{}\n",
            json!({"kind": "witness", "sequence": 1, "slot": "root-b", "boot_id": "boot-0"}),
            json!({"kind": "health", "sequence": 2, "boot_id": "boot-0", "status": "PASSED"})
        ),
    )
    .unwrap();
    assert_eq!(f.status(Some(&healthy))["witness_slot"], "root-b");
    assert_eq!(verdict(&f, Some(&healthy)), "ACTIVE");
    // A record cut off by power loss is ignored, not trusted.
    let torn = f.witness(&["root-a"], true);
    assert_eq!(f.status(Some(&torn))["witness_slot"], "root-a");
    let missing = f.dir.path().join("no-ledger.jsonl");
    assert_eq!(verdict(&f, Some(&missing)), "ACTIVE_UNCONFIRMED");
    std::fs::write(&missing, b"{\"slot\":").unwrap();
    assert_eq!(
        verdict(&f, Some(&missing)),
        "ACTIVE_UNCONFIRMED",
        "no newline, no evidence"
    );
    std::fs::write(&missing, b"not json\n").unwrap();
    let (image, disk) = f.open();
    assert!(status(&image, &disk, &f.esp, Some(&missing), false).is_err());
}

// ---- irreversible activation: the fallback is demoted (ADR-0023 R4, G4b) -------

const A_ENTRY: &str = "/loader/entries/flowerly-a.conf";
const A_MANUAL: &str = "/loader/entries/manual-flowerly-a.conf";
const B_ARMED: &str = "/loader/entries/flowerly-b+3.conf";

impl Fixture {
    fn publish(&mut self, fallback: Fallback, faults: &Faults) -> Result<Value> {
        let (_, disk) = self.open();
        publish_entry(
            &disk,
            Slot::A,
            Slot::B,
            &mut self.esp,
            3,
            &[],
            fallback,
            faults,
        )
    }

    fn roll_back(&mut self, to: Slot, restore: bool, faults: &Faults) -> Result<Value> {
        let (image, disk) = self.open();
        rollback(&image, &disk, to, &mut self.esp, restore, faults)
    }

    /// Whether the loader could pick the running root on its own while the
    /// target is armed: both would then match `default flowerly-*`.
    fn automatic_fallback_while_armed(&self) -> bool {
        self.esp.files.contains_key(A_ENTRY) && self.esp.files.contains_key(B_ARMED)
    }
}

#[test]
fn demoted_entries_are_recognised_and_never_match_the_loader_default() {
    assert_eq!(parse_demoted("manual-flowerly-a.conf"), Some(Slot::A));
    assert_eq!(parse_demoted("manual-flowerly-b.conf"), Some(Slot::B));
    for name in [
        "manual-flowerly-c.conf",
        "manual-flowerly-a+3.conf",
        "manual-flowerly-a.conf.tmp",
        "flowerly-a.conf",
        "manual-flowerly-ab.conf",
    ] {
        assert_eq!(parse_demoted(name), None, "{name}");
    }
    // The loader default is a prefix glob on the entry id.
    let glob = LOADER_DEFAULT.strip_prefix("default ").unwrap();
    let prefix = glob.strip_suffix('*').unwrap();
    assert!(!"manual-flowerly-a".starts_with(prefix));
    assert!(parse_entry("manual-flowerly-a.conf").is_none());
}

#[test]
fn an_irreversible_arm_demotes_the_fallback_and_then_publishes_the_target() {
    let mut f = fixture();
    f.stage(&Faults::none()).unwrap();
    let a_text = f.esp.files[A_ENTRY].clone();
    let report = f.publish(Fallback::ManualOnly, &Faults::none()).unwrap();
    assert_eq!(report["status"], "ARMED_PENDING");
    assert_eq!(report["fallback"], "MANUAL_ONLY");
    assert_eq!(report["demoted_entry"], "manual-flowerly-a.conf");
    assert!(!f.esp.files.contains_key(A_ENTRY));
    assert_eq!(
        f.esp.files[A_MANUAL], a_text,
        "the entry is kept for the menu"
    );
    assert!(f.esp.files.contains_key(B_ARMED));
    let status = f.status(None);
    assert_eq!(status["demoted_slots"], json!(["root-a"]));
    assert_eq!(status["verdict"], "PENDING");
    // A reversible arm keeps the automatic fallback.
    let mut f = fixture();
    f.stage(&Faults::none()).unwrap();
    let report = f.publish(Fallback::Automatic, &Faults::none()).unwrap();
    assert_eq!(report["fallback"], "AUTOMATIC");
    assert!(f.automatic_fallback_while_armed());
}

#[test]
fn an_interrupted_irreversible_arm_never_leaves_an_automatic_fallback() {
    for point in [
        "arm:after-tmp-entry",
        "arm:after-demote",
        "arm:after-rename",
    ] {
        let mut f = fixture();
        f.stage(&Faults::none()).unwrap();
        let a_text = f.esp.files[A_ENTRY].clone();
        assert!(err(f.publish(Fallback::ManualOnly, &Faults::error_at(point))).contains(point));
        assert!(!f.automatic_fallback_while_armed(), "{point}");
        // Re-running completes it (or finds it complete).
        let rerun = f.publish(Fallback::ManualOnly, &Faults::none());
        if point == "arm:after-rename" {
            assert!(err(rerun).contains("already has a boot entry"));
        } else {
            rerun.unwrap();
        }
        assert!(!f.automatic_fallback_while_armed(), "{point}");
        assert!(f.esp.files.contains_key(B_ARMED), "{point}");
        assert_eq!(f.esp.files[A_MANUAL], a_text, "{point}");
        assert!(!f.esp.files.keys().any(|k| k.ends_with(".tmp")), "{point}");
    }
}

#[test]
fn a_demoted_running_root_takes_no_reversible_arm() {
    let mut f = fixture();
    f.stage(&Faults::none()).unwrap();
    f.publish(Fallback::ManualOnly, &Faults::error_at("arm:after-demote"))
        .unwrap_err();
    let before = f.esp.files.clone();
    assert!(err(f.publish(Fallback::Automatic, &Faults::none()))
        .contains("no boot entry for the active slot"));
    assert_eq!(f.esp.files, before);
    // Both a regular and a demoted entry for one slot is not a state to guess in.
    let mut f = fixture();
    f.stage(&Faults::none()).unwrap();
    let a_text = f.esp.files[A_ENTRY].clone();
    f.esp.files.insert(A_MANUAL.into(), a_text);
    let before = f.esp.files.clone();
    for fallback in [Fallback::Automatic, Fallback::ManualOnly] {
        assert!(err(f.publish(fallback, &Faults::none())).contains("both"));
        assert_eq!(f.esp.files, before);
    }
}

#[test]
fn disarming_an_irreversible_arm_removes_the_target_before_restoring_the_fallback() {
    let mut f = fixture();
    f.stage(&Faults::none()).unwrap();
    let before = f.esp.files.clone();
    f.publish(Fallback::ManualOnly, &Faults::none()).unwrap();
    // Without permission to restore, a demoted root is not rolled back to.
    let armed = f.esp.files.clone();
    assert!(err(f.roll_back(Slot::A, false, &Faults::none())).contains("demoted"));
    assert_eq!(f.esp.files, armed);
    // Interrupted between the two steps: no target, fallback still manual.
    let point = "rollback:before-restore";
    assert!(err(f.roll_back(Slot::A, true, &Faults::error_at(point))).contains(point));
    assert!(!f.esp.files.contains_key(B_ARMED));
    assert!(!f.esp.files.contains_key(A_ENTRY));
    assert!(f.esp.files.contains_key(A_MANUAL));
    // Re-running restores it.
    let report = f.roll_back(Slot::A, true, &Faults::none()).unwrap();
    assert_eq!(report["restored_entry"], "flowerly-a.conf");
    assert_eq!(f.esp.files, before);
    assert!(err(f.roll_back(Slot::A, true, &Faults::none())).contains("nothing to roll back"));
}

#[test]
fn a_demoted_entry_blocks_restaging_until_it_is_removed() {
    let mut f = fixture();
    f.stage(&Faults::none()).unwrap();
    f.publish(Fallback::ManualOnly, &Faults::none()).unwrap();
    // Booted into B and blessed: B is now the running root, A is manual.
    let b = f.esp.files.remove(B_ARMED).unwrap();
    f.esp
        .files
        .insert("/loader/entries/flowerly-b.conf".into(), b);
    {
        let (image, mut disk) = f.open();
        let request = StageRequest {
            manifest_path: &f.manifest,
            active: Slot::B,
            target: Slot::A,
            candidate: &f.candidate,
            expected_sha: &f.candidate_sha,
        };
        assert!(
            err(stage(&image, &mut disk, &f.esp, &request, &Faults::none())).contains("boot entry")
        );
    }
    // Rolling back to the running B removes A's demoted entry.
    let report = f.roll_back(Slot::B, false, &Faults::none()).unwrap();
    assert_eq!(report["removed_entries"], json!(["manual-flowerly-a.conf"]));
    assert!(!f.esp.files.contains_key(A_MANUAL));
}

// ---- staged boot payload and baked identity (ADR-0023 G4) ---------------------

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn identity_value(slot: &str, partuuid: &str) -> Value {
    json!({
        "schema": IDENTITY_SCHEMA,
        "slot": slot,
        "root_partuuid": partuuid,
        "source_commit": COMMIT,
        "input_manifest_sha256": "d".repeat(64),
        "build_id": "build-b",
        "schema_version": 16,
        "min_reader": 16,
    })
}

impl Fixture {
    fn identity_file(&self, value: &Value) -> std::path::PathBuf {
        let path = self.dir.path().join("generation-root.json");
        std::fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
        path
    }

    fn stage_boot(&mut self, identity: &Value) -> Result<Value> {
        let identity = self.identity_file(identity);
        let (image, mut disk) = self.open();
        let request = BootPayload {
            kernel: &self.kernel,
            initramfs: &self.initramfs,
            identity: &identity,
        };
        stage_boot(
            &image,
            &mut disk,
            &self.manifest,
            Slot::B,
            &mut self.esp,
            &request,
            &Faults::none(),
        )
    }

    fn manifest_slot_b(&self) -> Part {
        Disk::load(&self.manifest).unwrap().slot(Slot::B).clone()
    }
}

#[test]
fn stage_boot_records_the_payload_and_identity_of_the_staged_slot() {
    let mut f = fixture();
    f.stage(&Faults::none()).unwrap();
    f.stage_boot(&identity_value("root-b", B_UUID)).unwrap();
    assert_eq!(f.esp.files["/flowerly/b/vmlinuz"], b"kernel-b");
    assert_eq!(f.esp.files["/flowerly/b/initramfs.img"], b"initramfs-b");
    let part = f.manifest_slot_b();
    let boot = part.boot.expect("boot payload recorded");
    assert_eq!(boot.kernel_sha256, sha(b"kernel-b"));
    assert_eq!(boot.initramfs_sha256, sha(b"initramfs-b"));
    let identity = part.identity.expect("identity recorded");
    assert_eq!(
        (identity.slot, identity.build_id.as_str()),
        (Slot::B, "build-b")
    );
    assert_eq!((identity.schema_version, identity.min_reader), (16, 16));
    // No boot entry: still only staged.
    assert!(!f.esp.files.keys().any(|k| k.contains("flowerly-b")));
}

#[test]
fn stage_boot_refuses_an_identity_that_is_not_this_slot() {
    let mut cases = Vec::new();
    cases.push(identity_value("root-a", B_UUID));
    cases.push(identity_value("root-b", A_UUID));
    let mut extra = identity_value("root-b", B_UUID);
    extra["extra"] = json!(1);
    cases.push(extra);
    let mut old = identity_value("root-b", B_UUID);
    old["schema"] = json!("flowerly-generation-root/v1");
    cases.push(old);
    let mut window = identity_value("root-b", B_UUID);
    window["min_reader"] = json!(17);
    cases.push(window);
    let mut missing = identity_value("root-b", B_UUID);
    missing.as_object_mut().unwrap().remove("min_reader");
    cases.push(missing);
    let mut commit = identity_value("root-b", B_UUID);
    commit["source_commit"] = json!("abc");
    cases.push(commit);
    for identity in cases {
        let mut f = fixture();
        f.stage(&Faults::none()).unwrap();
        let before = std::fs::read(&f.manifest).unwrap();
        assert!(f.stage_boot(&identity).is_err(), "{identity}");
        assert_eq!(std::fs::read(&f.manifest).unwrap(), before);
        assert!(!f.esp.files.contains_key("/flowerly/b/vmlinuz"));
    }
}

#[test]
fn stage_boot_needs_an_intact_staged_slot_without_a_boot_entry() {
    // Not staged at all.
    let mut f = fixture();
    assert!(err(f.stage_boot(&identity_value("root-b", B_UUID))).contains("staged"));
    // Slot bytes changed after staging.
    let mut f = fixture();
    f.stage(&Faults::none()).unwrap();
    {
        let (image, disk) = f.open();
        let part = disk.slot(Slot::B).clone();
        image.write_slot(&part, 4096, b"tampered").unwrap();
    }
    assert!(err(f.stage_boot(&identity_value("root-b", B_UUID))).contains("manifest hash"));
    // Already armed.
    let mut f = fixture();
    f.stage_and_arm();
    assert!(err(f.stage_boot(&identity_value("root-b", B_UUID))).contains("boot entry"));
}

#[test]
fn restaging_drops_the_previous_payload_record() {
    let mut f = fixture();
    f.stage(&Faults::none()).unwrap();
    f.stage_boot(&identity_value("root-b", B_UUID)).unwrap();
    f.stage(&Faults::none()).unwrap();
    let part = f.manifest_slot_b();
    assert!(part.boot.is_none() && part.identity.is_none());
}

// ---- the slot broker (ADR-0023 G4, R3) ----------------------------------------

use super::broker::{handle, serve_stream, BrokerConfig};
use std::os::unix::fs::PermissionsExt;

struct Broker {
    config: BrokerConfig,
    claim: Value,
    service_uid: u32,
}

impl Fixture {
    /// Staged slot B with payload and identity; a test-profile system booted
    /// from A whose flowerlyd knows schema 16; alice is an admin, bob is not.
    /// Their uids are far from any real account, so the service account
    /// (the uid running the tests) never shares one with them.
    fn broker(&mut self, target_identity: &Value) -> Broker {
        self.stage(&Faults::none()).unwrap();
        self.stage_boot(target_identity).unwrap();
        let meta = std::fs::metadata(self.dir.path()).unwrap();
        let (uid, gid) = (meta.uid(), meta.gid());
        let write = |name: &str, text: String| {
            let path = self.dir.path().join(name);
            std::fs::write(&path, text).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            path
        };
        let config = BrokerConfig {
            disk: self.disk.clone(),
            manifest: self.manifest.clone(),
            esp: self.dir.path().join("esp-unused"),
            running_identity: write(
                "running-identity.json",
                identity_value("root-a", A_UUID).to_string(),
            ),
            test_profile_marker: write("test-profile", String::new()),
            passwd: write(
                "passwd",
                format!(
                    "flowerly:x:{uid}:{gid}::/:/bin/false\nalice:x:4000001:4000001::/:/bin/false\nbob:x:4000002:4000002::/:/bin/false\ncarol:x:4000003:950::/:/bin/false\n"
                ),
            ),
            group: write("group", "flowerly-admin:x:950:alice\nusers:x:100:bob\n".into()),
            cmdline: write("cmdline", format!("console=ttyS0 root=PARTUUID={A_UUID} ro\n")),
            service_user: "flowerly".into(),
            admin_group: "flowerly-admin".into(),
            backup_dir: {
                let dir = self.dir.path().join("backups");
                std::fs::create_dir(&dir).unwrap();
                std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
                dir
            },
        };
        Broker {
            config,
            claim: json!({
                "rootfs_sha256": self.candidate_sha,
                "kernel_sha256": sha(b"kernel-b"),
                "initramfs_sha256": sha(b"initramfs-b"),
                "source_commit": COMMIT,
                "input_manifest_sha256": "d".repeat(64),
                "build_id": "build-b",
            }),
            service_uid: uid,
        }
    }
}

fn arm_intent(b: &Broker) -> Vec<u8> {
    json!({"version": 1, "intent": "arm", "slot": "root-b", "tries": 3,
           "admin_uid": 4000001, "generation": b.claim})
    .to_string()
    .into_bytes()
}

fn refused(f: &mut Fixture, b: &Broker, peer: u32, request: &[u8], needle: &str) {
    let esp_before = f.esp.files.clone();
    let disk_before = f.slices();
    let error = format!(
        "{:#}",
        handle(&b.config, peer, request, &mut f.esp, &Faults::none()).expect_err(needle)
    );
    assert!(error.contains(needle), "{error} should contain {needle}");
    assert_eq!(f.esp.files, esp_before, "{needle}: the ESP changed");
    assert_eq!(f.slices(), disk_before, "{needle}: the disk changed");
}

#[test]
fn the_broker_arms_a_verified_slot_and_writes_only_its_entry() {
    let mut f = fixture();
    let b = f.broker(&identity_value("root-b", B_UUID));
    let before = f.esp.files.clone();
    let disk_before = f.slices();
    let report = handle(
        &b.config,
        b.service_uid,
        &arm_intent(&b),
        &mut f.esp,
        &Faults::none(),
    )
    .unwrap();
    assert_eq!(report["status"], "ARMED_PENDING");
    let added: Vec<&String> = f
        .esp
        .files
        .keys()
        .filter(|k| !before.contains_key(*k))
        .collect();
    assert_eq!(added, ["/loader/entries/flowerly-b+3.conf"]);
    for (path, data) in &before {
        assert_eq!(&f.esp.files[path], data, "{path} changed");
    }
    assert_eq!(f.slices(), disk_before, "the broker never writes the disk");
}

#[test]
fn the_broker_refuses_everything_while_activation_is_disabled() {
    let mut f = fixture();
    let mut b = f.broker(&identity_value("root-b", B_UUID));
    b.config.test_profile_marker = f.dir.path().join("no-marker");
    let request = arm_intent(&b);
    refused(&mut f, &b, b.service_uid, &request, "disabled until Gate D");
    let disarm = br#"{"version":1,"intent":"disarm","admin_uid":4000001}"#;
    refused(&mut f, &b, b.service_uid, disarm, "disabled until Gate D");
}

#[test]
fn only_the_service_account_on_behalf_of_an_admin_is_heard() {
    let mut f = fixture();
    let b = f.broker(&identity_value("root-b", B_UUID));
    let request = arm_intent(&b);
    refused(
        &mut f,
        &b,
        b.service_uid + 1,
        &request,
        "not the service account",
    );
    for (admin, needle) in [(4000002, "not a member"), (4000999, "unknown uid")] {
        let mut intent: Value = serde_json::from_slice(&request).unwrap();
        intent["admin_uid"] = json!(admin);
        refused(
            &mut f,
            &b,
            b.service_uid,
            intent.to_string().as_bytes(),
            needle,
        );
    }
    // Membership by primary group counts.
    let mut intent: Value = serde_json::from_slice(&request).unwrap();
    intent["admin_uid"] = json!(4000003);
    handle(
        &b.config,
        b.service_uid,
        intent.to_string().as_bytes(),
        &mut f.esp,
        &Faults::none(),
    )
    .unwrap();
}

#[test]
fn malformed_intents_are_refused() {
    let mut f = fixture();
    let b = f.broker(&identity_value("root-b", B_UUID));
    let good: Value = serde_json::from_slice(&arm_intent(&b)).unwrap();
    let mut cases: Vec<(Vec<u8>, &str)> = vec![
        (b"not json".to_vec(), "intent"),
        (vec![b' '; 9000], "too large"),
    ];
    for (key, value, needle) in [
        ("version", json!(2), "version"),
        ("intent", json!("stage"), "intent"),
        ("extra", json!("x"), "unknown field"),
        ("tries", json!(0), "tries"),
        ("tries", json!(10), "tries"),
        ("slot", json!("root-a"), "slot"),
        ("slot", json!("/dev/vda2"), "slot"),
    ] {
        let mut intent = good.clone();
        intent[key] = value;
        cases.push((intent.to_string().into_bytes(), needle));
    }
    let mut no_claim = good.clone();
    no_claim.as_object_mut().unwrap().remove("generation");
    cases.push((no_claim.to_string().into_bytes(), "generation"));
    let mut path = good.clone();
    path["generation"]["kernel_path"] = json!("/tmp/evil");
    cases.push((path.to_string().into_bytes(), "unknown field"));
    for (request, needle) in cases {
        refused(&mut f, &b, b.service_uid, &request, needle);
    }
}

#[test]
fn the_claim_must_match_what_is_staged() {
    let mut f = fixture();
    let b = f.broker(&identity_value("root-b", B_UUID));
    let good: Value = serde_json::from_slice(&arm_intent(&b)).unwrap();
    for field in [
        "rootfs_sha256",
        "kernel_sha256",
        "initramfs_sha256",
        "source_commit",
        "input_manifest_sha256",
    ] {
        let mut intent = good.clone();
        let len = intent["generation"][field].as_str().unwrap().len();
        intent["generation"][field] = json!("f".repeat(len));
        refused(
            &mut f,
            &b,
            b.service_uid,
            intent.to_string().as_bytes(),
            field,
        );
    }
    let mut intent = good.clone();
    intent["generation"]["build_id"] = json!("build-x");
    refused(
        &mut f,
        &b,
        b.service_uid,
        intent.to_string().as_bytes(),
        "build_id",
    );
}

#[test]
fn the_broker_rechecks_the_disk_and_the_esp_itself() {
    // ESP payload changed after staging.
    let mut f = fixture();
    let b = f.broker(&identity_value("root-b", B_UUID));
    f.esp
        .files
        .insert("/flowerly/b/vmlinuz".into(), b"evil".to_vec());
    refused(&mut f, &b, b.service_uid, &arm_intent(&b), "vmlinuz");
    // Staged slot changed after staging.
    let mut f = fixture();
    let b = f.broker(&identity_value("root-b", B_UUID));
    {
        let (image, disk) = f.open();
        image
            .write_slot(&disk.slot(Slot::B).clone(), 0, b"tampered")
            .unwrap();
    }
    refused(&mut f, &b, b.service_uid, &arm_intent(&b), "manifest hash");
    // Active root changed.
    let mut f = fixture();
    let b = f.broker(&identity_value("root-b", B_UUID));
    {
        let (image, disk) = f.open();
        image
            .write_slot(&disk.slot(Slot::A).clone(), 0, b"tampered")
            .unwrap();
    }
    refused(&mut f, &b, b.service_uid, &arm_intent(&b), "active root");
    // Already armed.
    let mut f = fixture();
    let b = f.broker(&identity_value("root-b", B_UUID));
    handle(
        &b.config,
        b.service_uid,
        &arm_intent(&b),
        &mut f.esp,
        &Faults::none(),
    )
    .unwrap();
    refused(&mut f, &b, b.service_uid, &arm_intent(&b), "boot entry");
}

#[test]
fn a_slot_staged_without_payload_or_identity_is_not_armed() {
    let mut f = fixture();
    let b = f.broker(&identity_value("root-b", B_UUID));
    f.stage(&Faults::none()).unwrap(); // restaging drops the records
    refused(&mut f, &b, b.service_uid, &arm_intent(&b), "boot payload");
}

#[test]
fn an_incompatible_fallback_makes_the_activation_irreversible_and_refused() {
    let mut f = fixture();
    let mut target = identity_value("root-b", B_UUID);
    target["schema_version"] = json!(17);
    target["min_reader"] = json!(17);
    let b = f.broker(&target);
    refused(&mut f, &b, b.service_uid, &arm_intent(&b), "irreversible");
    // A window that still includes the fallback is fine.
    let mut f = fixture();
    let mut target = identity_value("root-b", B_UUID);
    target["schema_version"] = json!(17);
    target["min_reader"] = json!(16);
    let b = f.broker(&target);
    handle(
        &b.config,
        b.service_uid,
        &arm_intent(&b),
        &mut f.esp,
        &Faults::none(),
    )
    .unwrap();
}

#[test]
fn the_running_identity_must_be_the_active_slot() {
    for running in [
        identity_value("root-b", B_UUID),
        identity_value("root-a", B_UUID),
        json!({"schema": "flowerly-generation-root/v1"}),
    ] {
        let mut f = fixture();
        let b = f.broker(&identity_value("root-b", B_UUID));
        std::fs::write(&b.config.running_identity, running.to_string()).unwrap();
        refused(&mut f, &b, b.service_uid, &arm_intent(&b), "running");
    }
}

#[test]
fn disarm_removes_the_pending_entry_and_nothing_else() {
    let mut f = fixture();
    let b = f.broker(&identity_value("root-b", B_UUID));
    let disarm = br#"{"version":1,"intent":"disarm","admin_uid":4000001}"#;
    refused(&mut f, &b, b.service_uid, disarm, "nothing to roll back");
    let before = f.esp.files.clone();
    handle(
        &b.config,
        b.service_uid,
        &arm_intent(&b),
        &mut f.esp,
        &Faults::none(),
    )
    .unwrap();
    let report = handle(
        &b.config,
        b.service_uid,
        disarm,
        &mut f.esp,
        &Faults::none(),
    )
    .unwrap();
    assert_eq!(report["status"], "ROLLED_BACK");
    assert_eq!(f.esp.files, before);
}

#[test]
fn the_config_must_be_private_to_its_owner() {
    let f = fixture();
    let path = f.dir.path().join("broker.json");
    let config = json!({
        "disk": "/dev/vda", "manifest": "/efi/m.json", "esp": "/efi",
        "running_identity": "/usr/lib/flowerly/generation-root.json",
        "test_profile_marker": "/usr/lib/flowerly/generation-ab-test-profile",
        "passwd": "/etc/passwd", "group": "/etc/group", "cmdline": "/proc/cmdline",
        "service_user": "flowerly", "admin_group": "flowerly-admin",
        "backup_dir": "/var/lib/flowerly/activation-backups",
    });
    std::fs::write(&path, config.to_string()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(BrokerConfig::load(&path).unwrap().service_user, "flowerly");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664)).unwrap();
    assert!(format!("{:#}", BrokerConfig::load(&path).unwrap_err()).contains("writable"));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let mut extra = config.clone();
    extra["extra"] = json!(1);
    std::fs::write(&path, extra.to_string()).unwrap();
    assert!(BrokerConfig::load(&path).is_err());
}

#[test]
fn the_broker_answers_over_a_real_socket_with_kernel_peer_credentials() {
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixStream;
    let mut f = fixture();
    let mut b = f.broker(&identity_value("root-b", B_UUID));
    // A real ESP directory with the same files as the in-memory one.
    let esp_dir = f.dir.path().join("esp");
    for (path, data) in &f.esp.files {
        let target = esp_dir.join(path.trim_start_matches('/'));
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, data).unwrap();
    }
    b.config.esp = esp_dir.clone();
    let config_path = f.dir.path().join("broker.json");
    std::fs::write(&config_path, serde_json::to_string(&b.config).unwrap()).unwrap();
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600)).unwrap();

    let (client, server) = UnixStream::pair().unwrap();
    let path = config_path.clone();
    let worker = std::thread::spawn(move || serve_stream(server, &path));
    let mut request = arm_intent(&b);
    request.push(b'\n');
    std::io::Write::write_all(&mut &client, &request).unwrap();
    let mut line = String::new();
    BufReader::new(&client).read_line(&mut line).unwrap();
    worker.join().unwrap().unwrap();
    let reply: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(reply["ok"], true, "{reply}");
    assert!(esp_dir.join("loader/entries/flowerly-b+3.conf").is_file());

    // The same request from a socket the service account did not open is
    // refused with a reason, and the answer still arrives.
    let mut wrong = b.config.clone();
    wrong.service_user = "alice".into();
    std::fs::write(&config_path, serde_json::to_string(&wrong).unwrap()).unwrap();
    let (client, server) = UnixStream::pair().unwrap();
    let path = config_path.clone();
    let worker = std::thread::spawn(move || serve_stream(server, &path));
    std::io::Write::write_all(&mut &client, &request).unwrap();
    let mut line = String::new();
    BufReader::new(&client).read_line(&mut line).unwrap();
    worker.join().unwrap().unwrap();
    let reply: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(reply["ok"], false);
    assert!(
        reply["reason"]
            .as_str()
            .unwrap()
            .contains("service account"),
        "{reply}"
    );
}

#[test]
fn the_broker_trusts_no_manifest_it_did_not_protect() {
    // A manifest others may write is not the broker's.
    let mut f = fixture();
    let b = f.broker(&identity_value("root-b", B_UUID));
    std::fs::set_permissions(&f.manifest, std::fs::Permissions::from_mode(0o664)).unwrap();
    refused(&mut f, &b, b.service_uid, &arm_intent(&b), "disk manifest");
    // A recorded identity edited to name the other slot is refused even
    // though stage_boot would never have written it.
    let mut f = fixture();
    let b = f.broker(&identity_value("root-b", B_UUID));
    let mut raw: Value = serde_json::from_slice(&std::fs::read(&f.manifest).unwrap()).unwrap();
    raw["partitions"][2]["identity"]["slot"] = json!("root-a");
    std::fs::write(&f.manifest, serde_json::to_vec(&raw).unwrap()).unwrap();
    std::fs::set_permissions(&f.manifest, std::fs::Permissions::from_mode(0o600)).unwrap();
    refused(&mut f, &b, b.service_uid, &arm_intent(&b), "another slot");
}

// ---- the irreversible path through the broker (ADR-0023 R4, G4b) -------------

const SQLITE_MAGIC: &[u8] = b"SQLite format 3\0";

fn irreversible_target() -> Value {
    let mut target = identity_value("root-b", B_UUID);
    target["schema_version"] = json!(17);
    target["min_reader"] = json!(17);
    target
}

/// A backup the service account wrote: `<backup_dir>/<sha256>.sqlite`.
fn write_backup(b: &Broker, body: &[u8]) -> String {
    let data = [SQLITE_MAGIC, body].concat();
    let digest = sha(&data);
    let path = b.config.backup_dir.join(format!("{digest}.sqlite"));
    std::fs::write(&path, &data).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    digest
}

fn with_backup(b: &Broker, backup_sha256: &str) -> Vec<u8> {
    let mut intent: Value = serde_json::from_slice(&arm_intent(b)).unwrap();
    intent["irreversible"] = json!({ "backup_sha256": backup_sha256 });
    intent.to_string().into_bytes()
}

fn plan_intent(b: &Broker) -> Vec<u8> {
    json!({"version": 1, "intent": "plan", "slot": "root-b",
           "admin_uid": 4000001, "generation": b.claim})
    .to_string()
    .into_bytes()
}

fn answer(f: &mut Fixture, b: &Broker, request: &[u8]) -> Value {
    handle(
        &b.config,
        b.service_uid,
        request,
        &mut f.esp,
        &Faults::none(),
    )
    .unwrap()
}

#[test]
fn plan_verifies_everything_and_writes_nothing() {
    for (target, irreversible) in [
        (identity_value("root-b", B_UUID), false),
        (irreversible_target(), true),
    ] {
        let mut f = fixture();
        let b = f.broker(&target);
        let esp_before = f.esp.files.clone();
        let disk_before = f.slices();
        let manifest_before = std::fs::read(&f.manifest).unwrap();
        let plan = answer(&mut f, &b, &plan_intent(&b));
        assert_eq!(plan["status"], "PLANNED");
        assert_eq!(plan["target"], "root-b");
        assert_eq!(plan["root_partuuid"], B_UUID);
        assert_eq!(plan["irreversible"], irreversible);
        assert_eq!(plan["fallback_schema_version"], 16);
        assert_eq!(plan["target_min_reader"], target["min_reader"]);
        assert_eq!(f.esp.files, esp_before);
        assert_eq!(f.slices(), disk_before);
        assert_eq!(std::fs::read(&f.manifest).unwrap(), manifest_before);
    }
    // The slot may be left to the broker; a wrong one is refused.
    let mut f = fixture();
    let b = f.broker(&identity_value("root-b", B_UUID));
    let mut open: Value = serde_json::from_slice(&plan_intent(&b)).unwrap();
    open.as_object_mut().unwrap().remove("slot");
    assert_eq!(
        answer(&mut f, &b, open.to_string().as_bytes())["target"],
        "root-b"
    );
    let mut active: Value = serde_json::from_slice(&plan_intent(&b)).unwrap();
    active["slot"] = json!("root-a");
    refused(
        &mut f,
        &b,
        b.service_uid,
        active.to_string().as_bytes(),
        "inactive slot",
    );
    // An arm must name it.
    let mut unnamed: Value = serde_json::from_slice(&arm_intent(&b)).unwrap();
    unnamed.as_object_mut().unwrap().remove("slot");
    refused(
        &mut f,
        &b,
        b.service_uid,
        unnamed.to_string().as_bytes(),
        "inactive slot",
    );
    // A plan is checked like an arm: a wrong claim, an armed slot or an
    // acknowledgement are refused.
    let mut wrong: Value = serde_json::from_slice(&plan_intent(&b)).unwrap();
    wrong["generation"]["build_id"] = json!("build-x");
    refused(
        &mut f,
        &b,
        b.service_uid,
        wrong.to_string().as_bytes(),
        "build_id",
    );
    let mut ack: Value = serde_json::from_slice(&plan_intent(&b)).unwrap();
    ack["irreversible"] = json!({ "backup_sha256": "a".repeat(64) });
    refused(
        &mut f,
        &b,
        b.service_uid,
        ack.to_string().as_bytes(),
        "plan",
    );
    let mut tries: Value = serde_json::from_slice(&plan_intent(&b)).unwrap();
    tries["tries"] = json!(3);
    refused(
        &mut f,
        &b,
        b.service_uid,
        tries.to_string().as_bytes(),
        "plan",
    );
    answer(&mut f, &b, &arm_intent(&b));
    refused(&mut f, &b, b.service_uid, &plan_intent(&b), "boot entry");
}

#[test]
fn an_acknowledged_irreversible_activation_with_a_verified_backup_demotes_the_fallback() {
    let mut f = fixture();
    let b = f.broker(&irreversible_target());
    let a_text = f.esp.files[A_ENTRY].clone();
    let digest = write_backup(&b, b"state");
    let report = answer(&mut f, &b, &with_backup(&b, &digest));
    assert_eq!(report["status"], "ARMED_PENDING");
    assert_eq!(report["fallback"], "MANUAL_ONLY");
    assert_eq!(report["backup_sha256"], digest);
    assert!(!f.esp.files.contains_key(A_ENTRY));
    assert_eq!(f.esp.files[A_MANUAL], a_text);
    assert!(f.esp.files.contains_key(B_ARMED));
}

#[test]
fn an_irreversible_activation_without_a_sound_backup_is_refused() {
    let mut f = fixture();
    let b = f.broker(&irreversible_target());
    refused(
        &mut f,
        &b,
        b.service_uid,
        &arm_intent(&b),
        "acknowledgement",
    );
    // No such backup.
    refused(
        &mut f,
        &b,
        b.service_uid,
        &with_backup(&b, &"a".repeat(64)),
        "backup",
    );
    // Malformed hash: never a path.
    for bad in ["../../etc/shadow", "A".repeat(64).as_str(), "a"] {
        refused(
            &mut f,
            &b,
            b.service_uid,
            &with_backup(&b, bad),
            "backup_sha256",
        );
    }
    // Content that is not the named hash.
    let digest = write_backup(&b, b"state");
    let path = b.config.backup_dir.join(format!("{digest}.sqlite"));
    std::fs::write(&path, [SQLITE_MAGIC, b"other".as_slice()].concat()).unwrap();
    refused(
        &mut f,
        &b,
        b.service_uid,
        &with_backup(&b, &digest),
        "does not match",
    );
    // Not a SQLite database.
    let digest = sha(b"plain text");
    let path = b.config.backup_dir.join(format!("{digest}.sqlite"));
    std::fs::write(&path, b"plain text").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    refused(
        &mut f,
        &b,
        b.service_uid,
        &with_backup(&b, &digest),
        "SQLite",
    );
    // Readable by others.
    let digest = write_backup(&b, b"shared");
    let path = b.config.backup_dir.join(format!("{digest}.sqlite"));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    refused(
        &mut f,
        &b,
        b.service_uid,
        &with_backup(&b, &digest),
        "private",
    );
    // A symlink to a real backup.
    let digest = write_backup(&b, b"linked");
    let real = b.config.backup_dir.join(format!("{digest}.sqlite"));
    let moved = f.dir.path().join("elsewhere.sqlite");
    std::fs::rename(&real, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &real).unwrap();
    refused(
        &mut f,
        &b,
        b.service_uid,
        &with_backup(&b, &digest),
        "regular file",
    );
}

#[test]
fn a_reversible_activation_takes_no_irreversible_acknowledgement() {
    let mut f = fixture();
    let b = f.broker(&identity_value("root-b", B_UUID));
    let digest = write_backup(&b, b"state");
    refused(
        &mut f,
        &b,
        b.service_uid,
        &with_backup(&b, &digest),
        "reversible",
    );
}

#[test]
fn a_target_that_cannot_read_the_running_database_is_refused() {
    // The running root already migrated to a window starting at 17; a target
    // whose flowerlyd knows only 16 would not start (a rollback to it needs
    // the backup restored first).
    let mut f = fixture();
    let b = f.broker(&identity_value("root-b", B_UUID));
    let mut running = identity_value("root-a", A_UUID);
    running["schema_version"] = json!(17);
    running["min_reader"] = json!(17);
    std::fs::write(&b.config.running_identity, running.to_string()).unwrap();
    refused(&mut f, &b, b.service_uid, &arm_intent(&b), "cannot read");
    refused(&mut f, &b, b.service_uid, &plan_intent(&b), "cannot read");
}

#[test]
fn disarming_an_irreversible_arm_restores_the_running_root() {
    let mut f = fixture();
    let b = f.broker(&irreversible_target());
    let before = f.esp.files.clone();
    let digest = write_backup(&b, b"state");
    answer(&mut f, &b, &with_backup(&b, &digest));
    let disarm = br#"{"version":1,"intent":"disarm","admin_uid":4000001}"#;
    let report = answer(&mut f, &b, disarm);
    assert_eq!(report["restored_entry"], "flowerly-a.conf");
    assert_eq!(f.esp.files, before);
}

#[test]
fn the_test_image_config_is_one_the_broker_accepts() {
    // The JSON the test-image script writes, parsed as the broker parses it
    // (unknown or missing fields fail), pointing where flowerlyd looks.
    let script = include_str!("../../../../scripts/prepare-generation-ab-rootfs.sh");
    let json = script
        .lines()
        .skip_while(|l| !l.contains("slot-broker.json\" <<'EOF'"))
        .nth(1)
        .expect("the script writes the broker config");
    let config: BrokerConfig = serde_json::from_str(json).unwrap();
    assert_eq!(
        config.backup_dir,
        Path::new("/var/lib/flowerly/activation-backups")
    );
    assert_eq!(
        config.test_profile_marker,
        Path::new("/usr/lib/flowerly/generation-ab-test-profile")
    );
    assert_eq!(config.esp, Path::new("/efi"));
}
