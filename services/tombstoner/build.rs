fn main() {
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs");
    println!("cargo:rerun-if-env-changed=RB_BUILD_SHA");
    println!("cargo:rerun-if-env-changed=RB_BUILD_TIMESTAMP");
    println!("cargo:rerun-if-env-changed=RB_BUILD_DIRTY");
}
