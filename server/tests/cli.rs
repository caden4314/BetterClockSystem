use std::fs;

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use tempfile::tempdir;

fn valid_alarm_json() -> &'static str {
    r#"
{
  "version": 1,
  "alarms": [
    {
      "id": "wake-1",
      "enabled": true,
      "kind": "one_time",
      "local_datetime": "2099-02-07T07:30:00.000000000"
    },
    {
      "id": "standup-weekdays",
      "enabled": true,
      "kind": "recurring",
      "time_local": "09:30:00.000000000",
      "days_of_week": ["Mon", "Tue", "Wed", "Thu", "Fri"]
    }
  ]
}
"#
}

#[test]
fn diagnostics_succeeds_with_valid_alarm_file() {
    let dir = tempdir().expect("tempdir");
    let alarms = dir.path().join("alarms.json");
    fs::write(&alarms, valid_alarm_json()).expect("write json");

    let mut cmd = cargo_bin_cmd!("betterclock");
    cmd.arg("--diagnostics")
        .arg("--alarms")
        .arg(alarms)
        .assert()
        .success()
        .stdout(predicate::str::contains("Selected timing source"));
}

#[test]
fn malformed_json_fails_with_clear_error() {
    let dir = tempdir().expect("tempdir");
    let alarms = dir.path().join("alarms.json");
    fs::write(&alarms, "{ not-valid-json ").expect("write invalid json");

    let mut cmd = cargo_bin_cmd!("betterclock");
    cmd.arg("--diagnostics")
        .arg("--alarms")
        .arg(alarms)
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid JSON"));
}

#[test]
fn hardware_mode_fails_when_unavailable() {
    let dir = tempdir().expect("tempdir");
    let alarms = dir.path().join("alarms.json");
    fs::write(&alarms, valid_alarm_json()).expect("write json");

    let mut cmd = cargo_bin_cmd!("betterclock");
    cmd.arg("--diagnostics")
        .arg("--timing-source")
        .arg("hardware")
        .arg("--alarms")
        .arg(alarms)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "hardware timing source unavailable",
        ));
}

#[test]
fn auto_mode_reports_fallback_when_hardware_missing() {
    let dir = tempdir().expect("tempdir");
    let alarms = dir.path().join("alarms.json");
    fs::write(&alarms, valid_alarm_json()).expect("write json");

    let mut cmd = cargo_bin_cmd!("betterclock");
    cmd.arg("--diagnostics")
        .arg("--timing-source")
        .arg("auto")
        .arg("--alarms")
        .arg(alarms)
        .assert()
        .success()
        .stdout(predicate::str::contains("Fallback reason"));
}
