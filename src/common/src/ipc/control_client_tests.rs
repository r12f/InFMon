use super::*;

#[test]
fn new_stores_path() {
    let client = InFMonControlClient::new(Path::new("/tmp/test.sock"));
    assert_eq!(client.path(), Path::new("/tmp/test.sock"));
}
