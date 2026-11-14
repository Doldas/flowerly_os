//! Black-box tests of `distro-builder slot` (#48): the real executable on a
//! disk made by the real `build-flowerly-ab-disk.sh`, with real GPT, FAT (via
//! mtools) and ext4 images. Power loss is a real SIGKILL of the tool when the
//! `fault-injection` feature is enabled.
//!
//! Needs sfdisk, mkfs.vfat, mkfs.ext4, dumpe2fs and mtools. Without them the
//! tests skip, unless FLOWERLY_REQUIRE_AB_TOOLS=1 (CI), where they fail.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TOOLS: [&str; 9] = [
    "sfdisk",
    "mkfs.vfat",
    "mkfs.ext4",
    "dumpe2fs",
    "mmd",
    "mcopy",
    "mdir",
    "mtype",
    "mdel",
];

fn which(tool: &str) -> bool {
    let path = format!(
        "{}:/usr/sbin:/sbin",
        std::env::var("PATH").unwrap_or_default()
    );
    path.split(':').any(|d| Path::new(d).join(tool).is_file())
}

fn tools_available() -> bool {
    let missing: Vec<&str> = TOOLS.iter().copied().filter(|t| !which(t)).collect();
    if missing.is_empty() {
        return true;
    }
    assert!(
        std::env::var("FLOWERLY_REQUIRE_AB_TOOLS").as_deref() != Ok("1"),
        "FLOWERLY_REQUIRE_AB_TOOLS=1 but missing: {missing:?}"
    );
    eprintln!("skipping slot CLI test, missing tools: {missing:?}");
    false
}

fn run(cmd: &mut Command) -> Output {
    cmd.env(
        "PATH",
        format!(
            "{}:/usr/sbin:/sbin",
            std::env::var("PATH").unwrap_or_default()
        ),
    )
    .output()
    .unwrap()
}

fn ok(cmd: &mut Command) -> Output {
    let out = run(cmd);
    assert!(
        out.status.success(),
        "{cmd:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn sha(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap();
    Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

struct Rig {
    dir: tempfile::TempDir,
    a_uuid: String,
    b_uuid: String,
}

impl Rig {
    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let ext4 = |name: &str, label: &str| {
            let image = dir.path().join(name);
            std::fs::File::create(&image)
                .unwrap()
                .set_len(8 << 20)
                .unwrap();
            ok(Command::new("mkfs.ext4")
                .args(["-q", "-F", "-L", label])
                .arg(&image));
            image
        };
        let volume = ext4("root-a.ext4", "FLOWERLY_ROOT");
        ext4("root-b.ext4", "FLOWERLY_ROOT");
        let mut efi = b"MZ".to_vec();
        efi.resize(2048, 7);
        std::fs::write(dir.path().join("boot.efi"), efi).unwrap();
        std::fs::write(dir.path().join("vmlinuz"), b"kernel-a").unwrap();
        std::fs::write(dir.path().join("initrd"), b"initrd-a").unwrap();
        std::fs::write(dir.path().join("vmlinuz-b"), b"kernel-b").unwrap();
        std::fs::write(dir.path().join("initrd-b"), b"initrd-b").unwrap();
        let (a_uuid, b_uuid) = (
            "aaaaaaaa-1111-4111-8111-111111111111".to_owned(),
            "bbbbbbbb-2222-4222-8222-222222222222".to_owned(),
        );
        ok(Command::new("bash")
            .arg(root.join("scripts/build-flowerly-ab-disk.sh"))
            .args(["--volume"])
            .arg(&volume)
            .arg("--kernel")
            .arg(dir.path().join("vmlinuz"))
            .arg("--initramfs")
            .arg(dir.path().join("initrd"))
            .arg("--boot-efi")
            .arg(dir.path().join("boot.efi"))
            .arg("--output")
            .arg(dir.path().join("disk.img"))
            .env("FLOWERLY_ESP_MB", "64")
            .env("FLOWERLY_DATA_MB", "64")
            .env("FLOWERLY_AB_ROOT_A_PARTUUID", &a_uuid)
            .env("FLOWERLY_AB_ROOT_B_PARTUUID", &b_uuid));
        Self {
            dir,
            a_uuid,
            b_uuid,
        }
    }

    fn slot(&self, verb: &str, extra: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_distro-builder"));
        cmd.args(["slot", verb, "--disk"])
            .arg(self.path("disk.img"))
            .arg("--manifest")
            .arg(self.path("disk.img.manifest.json"))
            .args(["--active-partuuid", &self.a_uuid])
            .args(extra);
        cmd
    }

    fn json(&self, verb: &str, extra: &[&str]) -> Value {
        let out = ok(&mut self.slot(verb, extra));
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn stage_args(&self) -> Vec<String> {
        vec![
            "--slot".into(),
            "root-b".into(),
            "--candidate".into(),
            self.path("root-b.ext4").display().to_string(),
            "--sha256".into(),
            sha(&self.path("root-b.ext4")),
        ]
    }

    fn arm_args(&self) -> Vec<String> {
        [
            "--slot",
            "root-b",
            "--kernel",
            &self.path("vmlinuz-b").display().to_string(),
            "--initramfs",
            &self.path("initrd-b").display().to_string(),
            "--extra-option",
            "flowerly.test.fail_health=1",
        ]
        .map(str::to_owned)
        .to_vec()
    }

    fn esp(&self, tool: &str, args: &[&str]) -> String {
        let disk = self.path("disk.img");
        let offset = 1 << 20;
        let out = ok(Command::new(tool)
            .args(["-i", &format!("{}@@{offset}", disk.display())])
            .args(args));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .esp("mdir", &["-b", "::/loader/entries"])
            .lines()
            .filter_map(|l| l.rsplit('/').next().map(str::to_owned))
            .collect();
        names.sort();
        names
    }
}

fn refs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

#[test]
fn stage_arm_status_rollback_on_a_real_built_disk() {
    if !tools_available() {
        return;
    }
    let rig = Rig::new();
    // A fresh disk's slot A is still counting its first boot (never blessed).
    let fresh = rig.json("status", &[]);
    assert_eq!(fresh["pending_slots"], serde_json::json!(["root-a"]));
    let staged = rig.json("stage", &refs(&rig.stage_args()));
    assert_eq!(staged["status"], "STAGED_NOT_ARMED");
    assert_eq!(staged["candidate_sha256"], sha(&rig.path("root-b.ext4")));
    let armed = rig.json("arm", &refs(&rig.arm_args()));
    assert_eq!(armed["boot_entry"], "flowerly-b+3.conf");
    assert_eq!(rig.entries(), ["flowerly-a+3.conf", "flowerly-b+3.conf"]);
    let entry = rig.esp("mtype", &["::/loader/entries/flowerly-b+3.conf"]);
    assert!(entry.contains(&format!("root=PARTUUID={}", rig.b_uuid)));
    assert!(entry.contains("flowerly.test.fail_health=1"));
    assert_eq!(rig.esp("mtype", &["::/flowerly/b/vmlinuz"]), "kernel-b");
    let state = rig.json("status", &["--verify-slots"]);
    assert_eq!(state["verdict"], "PENDING");
    assert_eq!(
        state["pending_slots"],
        serde_json::json!(["root-a", "root-b"])
    );
    assert_eq!(state["slots"][1]["image_state"], "MATCHES_MANIFEST");
    // The witness decides ACTIVE/FELL_BACK, not the entries.
    let witness = rig.path("witness.jsonl");
    std::fs::write(&witness, "{\"sequence\":1,\"slot\":\"root-a\"}\n").unwrap();
    assert_eq!(
        rig.json("status", &["--witness", witness.to_str().unwrap()])["verdict"],
        "PENDING"
    );
    let rolled = rig.json("rollback", &["--to", "root-a"]);
    assert_eq!(rolled["status"], "ROLLED_BACK");
    assert_eq!(rig.entries(), ["flowerly-a+3.conf"]);
    assert_eq!(
        rig.json("status", &[])["pending_slots"],
        serde_json::json!(["root-a"])
    );
}

#[test]
fn refusals_are_nonzero_and_leave_the_image_byte_identical() {
    if !tools_available() {
        return;
    }
    let rig = Rig::new();
    let disk = rig.path("disk.img");
    let before = sha(&disk);
    let attempts: Vec<(&str, Vec<String>)> = vec![
        (
            "stage",
            [
                "--slot",
                "root-a",
                "--candidate",
                "/dev/null",
                "--sha256",
                &"0".repeat(64),
            ]
            .map(str::to_owned)
            .to_vec(),
        ),
        (
            "stage",
            [
                "--slot",
                "root-b",
                "--candidate",
                "/dev/null",
                "--sha256",
                &"0".repeat(64),
            ]
            .map(str::to_owned)
            .to_vec(),
        ),
        ("arm", rig.arm_args()),
        ("rollback", vec!["--to".into(), "root-a".into()]),
        (
            "stage",
            ["--slot", "root-c", "--candidate", "x", "--sha256", "y"]
                .map(str::to_owned)
                .to_vec(),
        ),
    ];
    for (verb, args) in attempts {
        let out = run(&mut rig.slot(verb, &refs(&args)));
        assert!(!out.status.success(), "{verb} {args:?} must be refused");
    }
    assert_eq!(sha(&disk), before);
    // A block device (or anything but a regular file) is never opened.
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_distro-builder"));
    cmd.args(["slot", "status", "--disk", "/dev/null", "--manifest"])
        .arg(rig.path("disk.img.manifest.json"));
    assert!(!run(&mut cmd).status.success());
    // The manifest must belong to this disk's real GPT.
    let wrong = rig.path("wrong.json");
    let text = std::fs::read_to_string(rig.path("disk.img.manifest.json")).unwrap();
    std::fs::write(
        &wrong,
        text.replace(&rig.b_uuid, "cccccccc-3333-4333-8333-333333333333"),
    )
    .unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_distro-builder"));
    cmd.args(["slot", "status", "--disk"])
        .arg(&disk)
        .arg("--manifest")
        .arg(&wrong);
    let out = run(&mut cmd);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("GPT differs"));
}

#[cfg(feature = "fault-injection")]
mod killed {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    fn killed_at(rig: &Rig, verb: &str, args: &[String], point: &str) {
        let out = run(rig
            .slot(verb, &refs(args))
            .env("FLOWERLY_SLOT_FAULT", point));
        assert_eq!(
            out.status.signal(),
            Some(9),
            "{point}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn arm_killed_at_every_step_never_exposes_a_half_written_entry() {
        if !tools_available() {
            return;
        }
        for (point, final_entry) in [
            ("arm:after-payload", false),
            ("arm:after-tmp-entry", false),
            ("arm:after-rename", true),
        ] {
            let rig = Rig::new();
            ok(&mut rig.slot("stage", &refs(&rig.stage_args())));
            killed_at(&rig, "arm", &rig.arm_args(), point);
            let names = rig.entries();
            assert_eq!(
                names.contains(&"flowerly-b+3.conf".to_owned()),
                final_entry,
                "{point}: {names:?}"
            );
            let state = rig.json("status", &[]);
            assert_eq!(
                state["pending_slots"].as_array().unwrap().len(),
                if final_entry { 2 } else { 1 },
                "{point}"
            );
            assert!(
                names.contains(&"flowerly-a+3.conf".to_owned()),
                "A stays bootable"
            );
            if !final_entry {
                assert_eq!(
                    names.contains(&"flowerly-b+3.conf.tmp".to_owned()),
                    point == "arm:after-tmp-entry"
                );
                // Re-running recovers without manual repair.
                assert_eq!(
                    rig.json("arm", &refs(&rig.arm_args()))["status"],
                    "ARMED_PENDING"
                );
                assert_eq!(rig.entries(), ["flowerly-a+3.conf", "flowerly-b+3.conf"]);
            }
        }
    }

    #[test]
    fn stage_killed_mid_write_leaves_the_old_manifest_and_a_rerunnable_slot() {
        if !tools_available() {
            return;
        }
        for point in [
            "stage:chunk:3",
            "stage:before-readback",
            "stage:before-manifest",
            "stage:manifest-temp",
        ] {
            let rig = Rig::new();
            let manifest = rig.path("disk.img.manifest.json");
            let before = std::fs::read(&manifest).unwrap();
            killed_at(&rig, "stage", &rig.stage_args(), point);
            assert_eq!(
                std::fs::read(&manifest).unwrap(),
                before,
                "{point}: manifest must be untouched"
            );
            let out = run(&mut rig.slot("arm", &refs(&rig.arm_args())));
            assert!(
                !out.status.success(),
                "{point}: a half-staged slot must not arm"
            );
            assert_eq!(
                rig.json("stage", &refs(&rig.stage_args()))["status"],
                "STAGED_NOT_ARMED",
                "{point}"
            );
        }
    }

    #[test]
    fn rollback_killed_after_a_delete_still_boots_a() {
        if !tools_available() {
            return;
        }
        let rig = Rig::new();
        ok(&mut rig.slot("stage", &refs(&rig.stage_args())));
        ok(&mut rig.slot("arm", &refs(&rig.arm_args())));
        killed_at(
            &rig,
            "rollback",
            &["--to".to_owned(), "root-a".to_owned()],
            "rollback:after-delete:1",
        );
        assert_eq!(rig.entries(), ["flowerly-a+3.conf"]);
        assert_eq!(
            rig.json("status", &[])["pending_slots"],
            serde_json::json!(["root-a"])
        );
    }
}
