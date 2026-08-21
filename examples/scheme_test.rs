use sourceview5::StyleSchemeManager;
fn main() {
    gtk::init().unwrap();
    let mgr = StyleSchemeManager::default();
    let ids = mgr.scheme_ids();
    println!(
        "schemes: {:?}",
        ids.iter().map(|s| s.to_string()).collect::<Vec<_>>()
    );
}
