fn main() {
    // Header is maintained manually at capi/include/cognee.h
    // cbindgen was useful for bootstrapping but the manual header
    // gives better control over the C API surface.

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "ios" {
        // iOS requires the Foundation framework at link time.
        println!("cargo:rustc-link-lib=framework=Foundation");

        // Help cc-rs find the iOS SDK if SDKROOT is not already set.
        if std::env::var("SDKROOT").is_err() {
            let sdk = if std::env::var("CARGO_CFG_TARGET_ABI")
                .unwrap_or_default()
                .contains("sim")
            {
                "iphonesimulator"
            } else {
                "iphoneos"
            };
            if let Ok(output) = std::process::Command::new("xcrun")
                .args(["--show-sdk-path", "--sdk", sdk])
                .output()
            {
                if output.status.success() {
                    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    println!("cargo:rustc-env=SDKROOT={path}");
                }
            }
        }
    }
}