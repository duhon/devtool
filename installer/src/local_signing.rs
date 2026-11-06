use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn captured(output: &Output) -> Value {
    json!({
        "success": output.status.success(),
        "exitCode": output.status.code(),
        "stdout": String::from_utf8_lossy(&output.stdout),
        "stderr": String::from_utf8_lossy(&output.stderr),
    })
}

fn snapshot(binary: &Path) -> Result<Value> {
    let bytes = fs::read(binary).context("Read staged executable for signing diagnostics")?;
    let signature = Command::new("/usr/bin/codesign")
        .args(["--display", "--verbose=4"])
        .arg(binary)
        .output()
        .context("Read staged executable signature")?;
    let attributes = Command::new("/usr/bin/xattr")
        .arg("-lx")
        .arg(binary)
        .output()
        .context("Read staged executable extended attributes")?;
    ensure!(
        attributes.status.success(),
        "Could not read staged executable extended attributes: {}",
        String::from_utf8_lossy(&attributes.stderr)
    );
    Ok(json!({
        "sha256": format!("{:x}", Sha256::digest(&bytes)),
        "signature": captured(&signature),
        "attributes": String::from_utf8_lossy(&attributes.stdout),
        "quarantine": super::quarantine::read(binary)?,
    }))
}

fn save(path: &Path, report: &Value) -> Result<()> {
    super::atomic_write(path, &serde_json::to_vec_pretty(report)?, 0o600)
        .context("Save local signing diagnostics")
}

pub(super) fn resign(binary: &Path, log: &Path) -> Result<()> {
    let mut report = json!({
        "operation": "local-ad-hoc-and-quarantine-removal-before-update-health-check",
        "installerVersion": env!("CARGO_PKG_VERSION"),
        "stagedExecutable": binary,
        "phase": "before-signing",
        "before": snapshot(binary)?,
    });
    save(log, &report)?;
    let signing = Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-"])
        .arg(binary)
        .output()
        .context("Start local ad-hoc signing of the staged executable")?;
    report["signing"] = captured(&signing);
    report["afterSigning"] = snapshot(binary)?;
    report["phase"] = json!("after-signing");
    save(log, &report)?;
    ensure!(
        signing.status.success(),
        "Local ad-hoc signing failed: {} (diagnostics: {})",
        String::from_utf8_lossy(&signing.stderr),
        log.display()
    );
    report["phase"] = json!("quarantine-removal");
    report["quarantineRemoval"] = json!({"attempted": false});
    save(log, &report)?;
    if super::quarantine::read(binary)?.is_some() {
        report["quarantineRemoval"]["attempted"] = json!(true);
        save(log, &report)?;
        let removal = Command::new("/usr/bin/xattr")
            .args(["-d", "com.apple.quarantine"])
            .arg(binary)
            .output()
            .context("Remove quarantine from the locally signed staged executable")?;
        report["quarantineRemoval"] = captured(&removal);
        report["quarantineRemoval"]["attempted"] = json!(true);
        save(log, &report)?;
        ensure!(
            removal.status.success(),
            "Staged executable quarantine removal failed: {} (diagnostics: {})",
            String::from_utf8_lossy(&removal.stderr),
            log.display()
        );
    }
    let verification = Command::new("/usr/bin/codesign")
        .args(["--verify", "--strict"])
        .arg(binary)
        .output()
        .context("Verify the locally signed executable")?;
    report["verification"] = captured(&verification);
    report["phase"] = json!("signature-verification");
    save(log, &report)?;
    ensure!(
        verification.status.success(),
        "Locally signed executable verification failed: {} (diagnostics: {})",
        String::from_utf8_lossy(&verification.stderr),
        log.display()
    );
    report["after"] = snapshot(binary)?;
    report["quarantineAbsent"] = json!(super::quarantine::read(binary)?.is_none());
    report["phase"] = json!("after-quarantine-removal");
    save(log, &report)?;
    ensure!(
        report["quarantineAbsent"] == true,
        "Staged executable still has quarantine after removal; update stopped (diagnostics: {})",
        log.display()
    );
    report["phase"] = json!("ready-for-health-check");
    save(log, &report)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resigns_an_unmarked_executable_without_requiring_quarantine() {
        let directory = tempfile::tempdir().unwrap();
        let binary = directory.path().join("services-devtools");
        let log = directory.path().join("signing.json");
        fs::copy(std::env::current_exe().unwrap(), &binary).unwrap();
        assert_eq!(super::super::quarantine::read(&binary).unwrap(), None);
        resign(&binary, &log).unwrap();
        assert_eq!(super::super::quarantine::read(&binary).unwrap(), None);
        let report: Value = serde_json::from_slice(&fs::read(&log).unwrap()).unwrap();
        assert_eq!(report["phase"], "ready-for-health-check");
        assert_eq!(report["signing"]["success"], true);
        assert_eq!(report["verification"]["success"], true);
        assert_eq!(report["quarantineRemoval"]["attempted"], false);
        assert_eq!(report["quarantineAbsent"], true);
        assert!(
            report["after"]["signature"]["stderr"]
                .as_str()
                .unwrap()
                .contains("Signature=adhoc")
        );
    }

    #[test]
    fn signs_a_quarantined_executable_then_removes_only_its_quarantine_and_launches_it() {
        let directory = tempfile::tempdir().unwrap();
        let binary = directory.path().join("services-devtools");
        let log = directory.path().join("signing.json");
        fs::copy(std::env::current_exe().unwrap(), &binary).unwrap();
        let marked = Command::new("/usr/bin/xattr")
            .args([
                "-w",
                "com.apple.quarantine",
                "0081;6ae242f3;Google Chrome for Testing;",
            ])
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            marked.status.success(),
            "{}",
            String::from_utf8_lossy(&marked.stderr)
        );
        let other_attribute = Command::new("/usr/bin/xattr")
            .args(["-w", "com.services-devtools.fixture", "keep"])
            .arg(&binary)
            .output()
            .unwrap();
        assert!(other_attribute.status.success());
        assert!(super::super::quarantine::read(&binary).unwrap().is_some());
        resign(&binary, &log).unwrap();
        let report: Value = serde_json::from_slice(&fs::read(&log).unwrap()).unwrap();
        assert_eq!(super::super::quarantine::read(&binary).unwrap(), None);
        assert_eq!(report["signing"]["success"], true);
        assert!(report["before"]["quarantine"].is_array());
        assert!(report["afterSigning"]["quarantine"].is_array());
        assert_eq!(report["quarantineRemoval"]["attempted"], true);
        assert_eq!(report["quarantineRemoval"]["success"], true);
        assert_eq!(report["quarantineAbsent"], true);
        assert!(report["after"]["quarantine"].is_null());
        assert_eq!(report["after"]["sha256"], report["afterSigning"]["sha256"]);
        assert_ne!(report["before"]["sha256"], report["after"]["sha256"]);
        assert_eq!(report["phase"], "ready-for-health-check");
        assert_eq!(report["verification"]["success"], true);
        let other_attribute = Command::new("/usr/bin/xattr")
            .args(["-p", "com.services-devtools.fixture"])
            .arg(&binary)
            .output()
            .unwrap();
        assert!(other_attribute.status.success());
        assert_eq!(
            String::from_utf8_lossy(&other_attribute.stdout).trim(),
            "keep"
        );
        let execution = Command::new(&binary).arg("--list").output().unwrap();
        assert!(
            execution.status.success(),
            "{}",
            String::from_utf8_lossy(&execution.stderr)
        );
    }

    #[test]
    fn missing_runtime_fails_instead_of_skipping_signing() {
        let directory = tempfile::tempdir().unwrap();
        assert!(
            resign(
                &directory.path().join("missing"),
                &directory.path().join("signing.json"),
            )
            .is_err()
        );
    }
}
