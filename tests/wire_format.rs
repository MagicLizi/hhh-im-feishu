use hhh_im_feishu::*;
use serde_json::{Value, json};

#[test]
fn unified_fixtures_round_trip_without_nulls_or_false_flags() {
    let mut count = 0;
    for entry in std::fs::read_dir(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/feishu"
    ))
    .unwrap()
    {
        let path = entry.unwrap().path();
        if !path.to_string_lossy().ends_with(".expected.json") {
            continue;
        }
        let expected: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let message: ImMessage = serde_json::from_value(expected.clone()).unwrap();
        assert_eq!(
            serde_json::to_value(&message).unwrap(),
            expected,
            "{}",
            path.display()
        );
        let restored: ImMessage =
            serde_json::from_value(serde_json::to_value(&message).unwrap()).unwrap();
        assert_eq!(message, restored);
        count += 1;
    }
    assert!(count >= 3, "不能用空 fixture 目录冒充对拍成功");
    let expected: Value = serde_json::from_str(include_str!("fixtures/send_request.json")).unwrap();
    let request: SendRequest = serde_json::from_value(expected.clone()).unwrap();
    assert_eq!(serde_json::to_value(request).unwrap(), expected);
}

#[test]
fn unknown_public_tags_are_rejected() {
    assert!(serde_json::from_value::<ImContent>(json!({"type":"future"})).is_err());
    // 卡片里的块、段落里的片与标题色调也一样：新版本写进来的类型，旧读法明确报错。
    assert!(serde_json::from_value::<CardBlock>(json!({"type":"future"})).is_err());
    assert!(serde_json::from_value::<CardInline>(json!({"type":"future"})).is_err());
    assert!(serde_json::from_value::<ImContent>(json!({"type":"card","tone":"future"})).is_err());
    assert!(serde_json::from_value::<SendTarget>(json!({"type":"future"})).is_err());
    assert!(serde_json::from_value::<SendOutcome>(json!({"type":"future"})).is_err());
    assert!(serde_json::from_value::<WebhookDelivery>(json!({"type":"future"})).is_err());
}
