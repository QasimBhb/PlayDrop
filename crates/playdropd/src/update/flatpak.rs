//! What a Flatpak is told when a newer release exists.
//!
//! `PlayDrop` is not on Flathub: each release carries a single-file bundle
//! (`playdrop-<version>-linux-x86_64.flatpak`, `.github/workflows/release.yml`), and a bundle
//! installed from a file never updates itself. The new bundle installs over the old one with
//! the same command as the first install (`docs/release/distribution.md`, "Flatpak";
//! `docs/guide/install.md`, "Updating"), which keeps the settings and pairings. The sentence
//! and the command are built here and nowhere else; the window, `playdrop update` and the
//! refusal of "Update" all show what the daemon sends (`docs/ipc.md` 1.11).

/// The bundle a release publishes for the Flatpak, `version` being `major.minor.patch`.
pub fn bundle_name(version: &str) -> String {
    format!("playdrop-{version}-linux-x86_64.flatpak")
}

/// The command that installs `version`'s bundle over the installed one, run in the folder
/// the bundle was saved to.
pub fn install_command(version: &str) -> String {
    format!("flatpak install --user ./{}", bundle_name(version))
}

/// The note under a Flatpak's offer of `version`. It ends with [`install_command`], so a
/// client that does not know `UpdateOffer::command` still shows the command.
pub fn reason(version: &str) -> String {
    format!(
        "A Flatpak cannot update itself. Download {} from the release page, then run this in the folder you saved it to: {}",
        bundle_name(version),
        install_command(version)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundle_name_is_the_one_the_release_publishes() {
        assert_eq!(
            bundle_name("0.16.0"),
            "playdrop-0.16.0-linux-x86_64.flatpak"
        );
    }

    #[test]
    fn the_command_installs_that_bundle_for_this_user() {
        assert_eq!(
            install_command("0.16.0"),
            "flatpak install --user ./playdrop-0.16.0-linux-x86_64.flatpak"
        );
    }

    #[test]
    fn the_reason_names_the_file_and_ends_with_the_command() {
        let r = reason("1.2.3");
        assert!(r.contains("Download playdrop-1.2.3-linux-x86_64.flatpak from the release page"));
        assert!(r.ends_with(&install_command("1.2.3")), "{r}");
        assert!(!r.contains('\u{2014}'), "no long dash in user text");
        let offer = playdrop_ipc::UpdateOffer {
            version: "1.2.3".into(),
            page_url: String::new(),
            can_install: false,
            reason: Some(r),
            command: Some(install_command("1.2.3")),
        };
        assert_eq!(
            offer.reason_before_command(),
            Some(
                "A Flatpak cannot update itself. Download playdrop-1.2.3-linux-x86_64.flatpak from the release page, then run this in the folder you saved it to:"
            )
        );
    }
}
