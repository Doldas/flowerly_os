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
        rollback(&image, &disk, Slot::A, &mut f.esp, &Faults::none()).unwrap()
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
        &Faults::none()
    ))
    .contains("nothing to roll back"));
    f.esp.files.remove("/loader/entries/flowerly-a.conf");
    assert!(err(rollback(
        &image,
        &disk,
        Slot::A,
        &mut f.esp,
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
            &Faults::error_at(point)
        ))
        .contains(point));
        assert!(
            esp.files.contains_key("/loader/entries/flowerly-a.conf"),
            "A stays bootable"
        );
        assert!(
            rollback(&image, &disk, Slot::A, &mut esp, &Faults::none()).is_err(),
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
