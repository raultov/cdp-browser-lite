use cdp_browser_lite::discovery::discover_default;

#[test]
#[ignore = "requires Chrome installed on the machine"]
fn given_real_machine_when_discovering_then_finds_executable() {
    let path = discover_default().expect("should find Chrome on this machine");
    assert!(path.is_file(), "discovered path must be a file: {path:?}");
}
