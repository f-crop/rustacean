fn main() {
    // Ensure control-api is re-linked when git HEAD or build-provenance env
    // vars change.  The actual SHA/timestamp capture happens in rb-build-info's
    // build.rs; these directives ensure the binary is considered stale whenever
    // the provenance inputs change so a fresh link picks up the updated values.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs");
    println!("cargo:rerun-if-env-changed=RB_BUILD_SHA");
    println!("cargo:rerun-if-env-changed=RB_BUILD_TIMESTAMP");
    println!("cargo:rerun-if-env-changed=RB_BUILD_DIRTY");
}
