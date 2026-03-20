use jasmine_core::{CoreError, DeviceId};
use uuid::Uuid;

#[mockall::automock]
trait GreetingService {
    fn greeting(&self, name: &str) -> String;
}

#[test]
fn hello_world_test() {
    let id = DeviceId(Uuid::new_v4());
    let error = CoreError::NotImplemented("hello");

    let mut service = MockGreetingService::new();
    service
        .expect_greeting()
        .with(mockall::predicate::eq("world"))
        .returning(|name| format!("hello {name}"));

    assert_eq!(error.to_string(), "operation not implemented: hello");
    assert_eq!(id.0.get_version_num(), 4);
    assert_eq!(service.greeting("world"), "hello world");
}
