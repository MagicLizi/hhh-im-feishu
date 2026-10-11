//! 插件清单 `hhh-im-provider.json` 按协议包的清单类型读得懂、合规，并与飞书的平台事实对得上。

use serde_json::Value;

const MANIFEST: &str = include_str!("../hhh-im-provider.json");

#[test]
fn the_manifest_uses_the_protocol_field_names_and_passes_its_checks() {
    let manifest: hhh_im_protocol::Manifest = serde_json::from_str(MANIFEST).unwrap();
    assert!(hhh_im_protocol::Manifest::id_ok(&manifest.id));
    assert!(manifest.im_type_ok());
    assert!(manifest.logo_ok());
    manifest.release_fields_ok().unwrap();
    // 清单类型不拒收不认识的字段：原文的每一项再写回去都还在、值也一样，才说明每个字段名都写对了。
    let raw: Value = serde_json::from_str(MANIFEST).unwrap();
    let written = serde_json::to_value(&manifest).unwrap();
    for (key, value) in raw.as_object().unwrap() {
        assert_eq!(written.get(key), Some(value), "清单字段 {key} 协议不认");
    }
}

#[test]
fn the_manifest_matches_the_feishu_platform_facts() {
    let manifest: hhh_im_protocol::Manifest = serde_json::from_str(MANIFEST).unwrap();
    let info = hhh_im_feishu::facts::INFO;
    assert_eq!(manifest.name, info.name);
    assert_eq!(manifest.im_type.as_deref(), Some(info.id));
    assert_eq!(manifest.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(manifest.command, env!("CARGO_PKG_NAME"));
    let logo = info.logo.unwrap();
    assert_eq!(
        manifest.logo_style,
        Some(hhh_im_protocol::platform::LogoStyle::Mark)
    );
    assert_eq!(logo.style, hhh_im_feishu::LogoStyle::Mark);
    let ink = manifest.logo_ink.unwrap();
    let facts = logo.ink.unwrap();
    for (wire, fact) in [
        (ink.x, facts.x),
        (ink.y, facts.y),
        (ink.width, facts.width),
        (ink.height, facts.height),
    ] {
        assert!((wire - f64::from(fact)).abs() < 1e-4, "{wire} ≠ {fact}");
    }
    // 清单里的标志文件与库里随代码带的是同一张图。
    assert_eq!(
        include_bytes!("../logo.svg").as_slice(),
        logo.bytes,
        "logo.svg 与 src/feishu/logo.svg 不一致"
    );
}
