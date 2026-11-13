//! `lit config`: settings kept in the database's `meta` table, and the
//! resolution of patient mode from flags and settings (ADR-012).
//!
//! Settings live in the database `lit` already opens, so they add no file,
//! search path or environment variable (ADR-003 rejects a configuration file).

use crate::db::Db;
use crate::http::{Patience, DEFAULT_PATIENCE_BUDGET};
use std::path::PathBuf;
use std::time::Duration;

/// A setting `lit config` accepts.
pub struct Setting {
    pub name: &'static str,
    /// Value used when the setting is not stored.
    pub default: &'static str,
    validate: fn(&str) -> Result<(), String>,
}

/// `on` makes every invocation patient, as `--patient` does.
pub const PATIENT: &str = "patient";
/// Seconds of retry sleep per patient Semantic Scholar request.
pub const PATIENT_BUDGET: &str = "patient_budget";

/// Every setting, in display order.
pub const SETTINGS: &[Setting] = &[
    Setting { name: PATIENT, default: "off", validate: validate_switch },
    // Equal to `DEFAULT_PATIENCE_BUDGET`; a test holds them together.
    Setting { name: PATIENT_BUDGET, default: "300", validate: validate_seconds },
];

/// Prefix of setting keys in the `meta` table, keeping them apart from
/// internal keys such as `schema_version`.
const META_PREFIX: &str = "config.";

fn validate_switch(value: &str) -> Result<(), String> {
    match value {
        "on" | "off" => Ok(()),
        _ => Err(format!("expected on or off, got {:?}", value)),
    }
}

fn validate_seconds(value: &str) -> Result<(), String> {
    parse_seconds(value).map(|_| ())
}

/// A positive whole number of seconds.
fn parse_seconds(value: &str) -> Result<Duration, String> {
    match value.parse::<u64>() {
        Ok(n) if n > 0 => Ok(Duration::from_secs(n)),
        _ => Err(format!("expected a positive whole number of seconds, got {:?}", value)),
    }
}

fn setting(name: &str) -> Result<&'static Setting, String> {
    SETTINGS.iter().find(|s| s.name == name).ok_or_else(|| {
        let names: Vec<&str> = SETTINGS.iter().map(|s| s.name).collect();
        format!("unknown setting {:?}; settings are {}", name, names.join(", "))
    })
}

fn meta_key(setting: &Setting) -> String {
    format!("{}{}", META_PREFIX, setting.name)
}

/// The stored value of setting `name`, if any.
pub fn get(db: &Db, name: &str) -> Result<Option<String>, String> {
    Ok(db.meta_get(&meta_key(setting(name)?)))
}

/// Validate and store `value` for setting `name`.
pub fn set(db: &Db, name: &str, value: &str) -> Result<(), String> {
    let s = setting(name)?;
    (s.validate)(value).map_err(|e| format!("{}: {}", name, e))?;
    db.meta_set(&meta_key(s), value).map_err(|e| e.to_string())
}

/// Remove the stored value of setting `name`, restoring its default.
pub fn unset(db: &Db, name: &str) -> Result<(), String> {
    db.meta_delete(&meta_key(setting(name)?)).map_err(|e| e.to_string())
}

/// The patient-mode flags of one invocation.
#[derive(Clone, Copy, Debug, Default)]
pub struct PatienceFlags {
    pub patient: bool,
    pub no_patient: bool,
    /// `--patient-budget`, in seconds; implies `--patient`.
    pub budget_secs: Option<u64>,
}

/// Patient mode for one invocation: `--no-patient` turns it off; otherwise
/// `--patient`, `--patient-budget` or the `patient` setting turns it on, with
/// the budget from `--patient-budget`, else `patient_budget`, else 300s.
/// Semantic Scholar requests are paced through `pacing_file`.
pub fn resolve_patience(db: &Db, flags: PatienceFlags, pacing_file: PathBuf) -> Result<Option<Patience>, String> {
    if flags.no_patient {
        return Ok(None);
    }
    let on = flags.patient || flags.budget_secs.is_some() || get(db, PATIENT)?.as_deref() == Some("on");
    if !on {
        return Ok(None);
    }
    let budget = match (flags.budget_secs, get(db, PATIENT_BUDGET)?) {
        (Some(secs), _) => Duration::from_secs(secs),
        (None, Some(stored)) => parse_seconds(&stored).map_err(|e| format!("stored {}: {}", PATIENT_BUDGET, e))?,
        (None, None) => DEFAULT_PATIENCE_BUDGET,
    };
    Ok(Some(Patience { budget, pacing_file }))
}

/// `lit config`: one `name = value` line per setting, marked `(default)`
/// when not stored.
pub fn list(db: &Db) -> Result<String, String> {
    let mut out = String::new();
    for s in SETTINGS {
        match get(db, s.name)? {
            Some(value) => out.push_str(&format!("{} = {}\n", s.name, value)),
            None => out.push_str(&format!("{} = {} (default)\n", s.name, s.default)),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        Db::open_in_memory().unwrap()
    }

    fn resolve(db: &Db, flags: PatienceFlags) -> Option<Patience> {
        resolve_patience(db, flags, "pace".into()).unwrap()
    }

    fn on() -> PatienceFlags {
        PatienceFlags { patient: true, ..Default::default() }
    }

    #[test]
    fn patience_is_off_without_flags_or_settings() {
        assert_eq!(resolve(&db(), PatienceFlags::default()), None);
    }

    #[test]
    fn patient_flag_turns_it_on_with_the_default_budget() {
        let p = resolve(&db(), on()).unwrap();
        assert_eq!(p, Patience { budget: Duration::from_secs(300), pacing_file: "pace".into() });
    }

    #[test]
    fn budget_flag_implies_patient_and_sets_the_budget() {
        let flags = PatienceFlags { budget_secs: Some(45), ..Default::default() };
        assert_eq!(resolve(&db(), flags).unwrap().budget, Duration::from_secs(45));
    }

    #[test]
    fn patient_setting_turns_it_on_and_no_patient_overrides_it() {
        let db = db();
        set(&db, PATIENT, "on").unwrap();
        set(&db, PATIENT_BUDGET, "120").unwrap();
        assert_eq!(resolve(&db, PatienceFlags::default()).unwrap().budget, Duration::from_secs(120));
        assert_eq!(resolve(&db, PatienceFlags { no_patient: true, ..Default::default() }), None);
        let flags = PatienceFlags { budget_secs: Some(10), ..Default::default() };
        assert_eq!(resolve(&db, flags).unwrap().budget, Duration::from_secs(10), "the flag beats the setting");
    }

    #[test]
    fn budget_setting_alone_does_not_turn_patience_on() {
        let db = db();
        set(&db, PATIENT_BUDGET, "120").unwrap();
        assert_eq!(resolve(&db, PatienceFlags::default()), None);
        assert_eq!(resolve(&db, on()).unwrap().budget, Duration::from_secs(120));
    }

    #[test]
    fn set_rejects_unknown_settings_and_bad_values() {
        let db = db();
        assert!(set(&db, "schema_version", "9").unwrap_err().contains("unknown setting"));
        assert!(set(&db, PATIENT, "yes").is_err());
        assert!(set(&db, PATIENT_BUDGET, "-1").is_err());
        assert!(set(&db, PATIENT_BUDGET, "0").is_err());
        assert_eq!(db.meta_get("schema_version").as_deref(), Some("2"), "internal keys are untouched");
    }

    #[test]
    fn settings_are_namespaced_and_unset_restores_the_default() {
        let db = db();
        set(&db, PATIENT, "on").unwrap();
        assert_eq!(db.meta_get("config.patient").as_deref(), Some("on"));
        assert_eq!(get(&db, PATIENT).unwrap().as_deref(), Some("on"));
        unset(&db, PATIENT).unwrap();
        assert_eq!(get(&db, PATIENT).unwrap(), None);
        assert_eq!(resolve(&db, PatienceFlags::default()), None);
    }

    #[test]
    fn listed_budget_default_is_the_resolved_default() {
        let listed = setting(PATIENT_BUDGET).unwrap().default;
        assert_eq!(parse_seconds(listed).unwrap(), DEFAULT_PATIENCE_BUDGET);
    }

    #[test]
    fn list_marks_defaults() {
        let db = db();
        set(&db, PATIENT, "on").unwrap();
        assert_eq!(list(&db).unwrap(), "patient = on\npatient_budget = 300 (default)\n");
    }
}
