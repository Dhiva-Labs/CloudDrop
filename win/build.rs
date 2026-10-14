fn main() {
    // Icon resource: the File Explorer sync root and the notification area use it.
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/agent.rc");
        println!("cargo:rerun-if-changed=assets/clouddrop.ico");
        embed_resource::compile("assets/agent.rc", embed_resource::NONE)
            .manifest_optional()
            .expect("icon resource");
    }
}
