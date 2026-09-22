//! Which version may replace which.

/// An update must never replace a newer UwURDP with an older (validly signed)
/// setup. The app checks the signature's file name against the feed's version;
/// this is the last line. In update mode it fails closed: when it can't tell
/// what is installed, it doesn't install.
pub fn check_not_older(installed: Option<&str>, update: &str) -> Result<(), String> {
    let parse = |version: &str| semver::Version::parse(version.trim()).ok();
    match (installed.and_then(parse), parse(update)) {
        (Some(installed), Some(update)) if update < installed => Err(format!(
            "UwURDP {installed} is already installed. This update is older ({update}), so it was skipped."
        )),
        (Some(_), Some(_)) => Ok(()),
        _ => Err("It isn't clear which UwURDP version is installed, so the update was skipped. Run the setup by hand instead.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updates_never_go_back() {
        assert!(check_not_older(Some("0.3.0"), "0.2.0-beta.1").is_err());
        assert!(check_not_older(Some("0.2.0"), "0.2.0-beta.1").is_err());
        assert!(check_not_older(Some("0.2.0-beta.1"), "0.2.0-beta.2").is_ok());
        assert!(
            check_not_older(Some("0.2.0-beta.1"), "0.2.0-beta.1").is_ok(),
            "repairing is fine"
        );
        assert!(check_not_older(None, "0.2.0").is_err(), "unknown means no");
        assert!(check_not_older(Some("unknown"), "0.2.0").is_err());
    }
}
