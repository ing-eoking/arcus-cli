use std::env;
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Os {
    Rhel,
    Debian,
    Mac,
    Other,
}

pub fn detect_os() -> Os {
    if cfg!(target_os = "macos") {
        return Os::Mac;
    }
    let text = std::fs::read_to_string("/etc/os-release").unwrap_or_default().to_lowercase();
    let ids: String = text.lines()
        .filter(|l| l.starts_with("id=") || l.starts_with("id_like="))
        .collect::<Vec<_>>().join(" ");
    if ["rhel", "centos", "fedora", "rocky", "almalinux"].iter().any(|k| ids.contains(k)) {
        Os::Rhel
    } else if ["debian", "ubuntu"].iter().any(|k| ids.contains(k)) {
        Os::Debian
    } else {
        Os::Other
    }
}

/// Returns required build tools for which `have` is false (`libtool` is probed as `glibtool` on macOS).
pub fn missing_tools(have: &dyn Fn(&str) -> bool) -> Vec<&'static str> {
    let mac = cfg!(target_os = "macos");
    ["cc", "make", "libtool", "perl", "autoconf", "automake"]
        .into_iter()
        .filter(|&t| {
            let probe = if mac && t == "libtool" { "glibtool" } else { t };
            !have(probe)
        })
        .collect()
}

/// Whether `name` resolves to an executable file in a PATH entry.
pub fn on_path(name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    env::var_os("PATH").map_or(false, |paths| {
        env::split_paths(&paths).any(|d| {
            std::fs::metadata(Path::new(&d).join(name))
                .map_or(false, |m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    })
}

/// Install guidance shown to the user; never executed.
pub fn install_hint(os: Os, missing: &[&str]) -> String {
    match os {
        Os::Rhel => "sudo dnf install gcc make libtool perl autoconf automake".into(),
        Os::Debian => "sudo apt-get install build-essential libtool perl autoconf automake".into(),
        Os::Mac => {
            let mut s = String::from("brew install libtool autoconf automake");
            if missing.iter().any(|t| *t == "cc" || *t == "make") {
                s.push_str("\nxcode-select --install");
            }
            s
        }
        Os::Other => format!("Install these with your package manager: {}", missing.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hints() {
        assert!(install_hint(Os::Mac, &["cc"]).contains("xcode-select --install"));
        assert!(install_hint(Os::Rhel, &["libtool"]).contains("dnf install"));
    }
    #[test]
    fn missing_uses_glibtool_on_mac_and_reports_absent() {
        let m = missing_tools(&|t| t != "perl" && t != "make");
        assert_eq!(m, ["make", "perl"]);
    }
}
