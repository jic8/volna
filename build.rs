use std::path::PathBuf;
use std::process::Command;

fn main() {
    glib_build_tools::compile_resources(
        &["data"],
        "data/resources.gresource.xml",
        "resources.gresource",
    );
    compile_translations();
}

/// Для разработки .mo собираются в OUT_DIR; при установке (flatpak) каталог
/// задаётся через VOLNA_LOCALEDIR, а .mo ставит сборочный скрипт.
fn compile_translations() {
    println!("cargo:rerun-if-changed=po");
    println!("cargo:rerun-if-env-changed=VOLNA_LOCALEDIR");

    if let Ok(dir) = std::env::var("VOLNA_LOCALEDIR") {
        println!("cargo:rustc-env=VOLNA_LOCALEDIR={dir}");
        return;
    }

    let locale_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("locale");
    let linguas = std::fs::read_to_string("po/LINGUAS").unwrap_or_default();
    for lang in linguas.split_whitespace().filter(|l| !l.starts_with('#')) {
        let out = locale_dir.join(lang).join("LC_MESSAGES");
        std::fs::create_dir_all(&out).unwrap();
        let status = Command::new("msgfmt")
            .arg(format!("po/{lang}.po"))
            .arg("-o")
            .arg(out.join("volna.mo"))
            .status();
        if !matches!(status, Ok(s) if s.success()) {
            println!("cargo:warning=msgfmt failed for {lang}, translation skipped");
        }
    }
    println!("cargo:rustc-env=VOLNA_LOCALEDIR={}", locale_dir.display());
}
