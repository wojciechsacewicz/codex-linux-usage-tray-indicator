fn main() {
    pkg_config::Config::new()
        .probe("gtk+-3.0")
        .expect("gtk+-3.0 is required");
    pkg_config::Config::new()
        .probe("ayatana-appindicator3-0.1")
        .expect("ayatana-appindicator3-0.1 is required");
    pkg_config::Config::new()
        .probe("gtk-layer-shell-0")
        .expect("gtk-layer-shell-0 is required");
}
