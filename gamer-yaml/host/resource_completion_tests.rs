use super::resources::validate_function_library_file;
use super::yaml_extension::YAML_EXTENSION_ID;
use crate::resources::{PackageInput, PackageStore};

#[test]
fn split_libraries_reject_duplicate_names_and_removed_references() {
    let data = tempfile::tempdir().unwrap();
    let store = PackageStore::open(&crate::config::Config {
        data_dir: data.path().into(),
        ..Default::default()
    })
    .unwrap();
    store
        .create_package(PackageInput {
            id: "qa".into(),
            android_targets: vec!["*".into()],
            ..Default::default()
        })
        .unwrap();
    let file = "automations/_function_extra.yaml";
    store
        .write_text(
            "qa",
            YAML_EXTENSION_ID,
            file,
            "version: 2\nfunctions:\n  helper:\n    run: []\n",
            None,
            false,
        )
        .unwrap();
    store
        .write_text(
            "qa",
            YAML_EXTENSION_ID,
            "automations/main.yaml",
            "version: 2\nrun:\n  - helper: {}\n",
            None,
            false,
        )
        .unwrap();
    let error = validate_function_library_file(&store, "qa", file, "version: 2\nfunctions: {}\n")
        .unwrap_err();
    assert_eq!(error[0]["code"], "yaml.functions.referenced");
    assert!(error[0]["message"].as_str().unwrap().contains("main.yaml"));
    assert!(error[0]["message"]
        .as_str()
        .unwrap()
        .contains("自动化“main.yaml” → 第 1 步"));
    let duplicate = validate_function_library_file(
        &store,
        "qa",
        "automations/_function.yaml",
        "version: 2\nfunctions:\n  helper:\n    run: []\n",
    )
    .unwrap_err();
    assert_eq!(duplicate[0]["code"], "yaml.fn.duplicate");
    store
        .delete_resource("qa", YAML_EXTENSION_ID, "automations/main.yaml")
        .unwrap();
    assert!(
        validate_function_library_file(&store, "qa", file, "version: 2\nfunctions: {}\n").is_ok()
    );
}

#[test]
fn removed_function_references_identify_callers_and_nested_steps() {
    let data = tempfile::tempdir().unwrap();
    let store = PackageStore::open(&crate::config::Config {
        data_dir: data.path().into(),
        ..Default::default()
    })
    .unwrap();
    store
        .create_package(PackageInput {
            id: "qa".into(),
            android_targets: vec!["*".into()],
            ..Default::default()
        })
        .unwrap();
    let file = "automations/_function.yaml";
    let remaining = r#"version: 2
functions:
  账号日常:
    run:
      - log: start
      - sleep: 1s
      - 账号登录: {}
      - if: true
        then:
          - 账号登录: {}
        else:
          - repeat: 2
            do:
              - 账号登录: {}
      - match_templates:
          cases:
            - template: login.png
              do:
                - 账号登录: {}
          else:
            - 账号登录: {}
"#;
    let original = format!("{remaining}  账号登录:\n    run: []\n");
    store
        .write_text("qa", YAML_EXTENSION_ID, file, &original, None, false)
        .unwrap();
    store
        .write_text(
            "qa",
            YAML_EXTENSION_ID,
            "automations/_function_extra.yaml",
            "version: 2\nfunctions:\n  另一个函数:\n    run:\n      - 账号登录: {}\n",
            None,
            false,
        )
        .unwrap();
    store
        .write_text(
            "qa",
            YAML_EXTENSION_ID,
            "automations/daily.yaml",
            "version: 2\nname: 每日自动化\nrun:\n  - 账号登录: {}\n",
            None,
            false,
        )
        .unwrap();

    // 删除与重命名均指出全部调用位置；同文件检查使用待保存的新内容。
    for candidate in [
        remaining.to_string(),
        format!("{remaining}  新登录:\n    run: []\n"),
    ] {
        let error = validate_function_library_file(&store, "qa", file, &candidate).unwrap_err();
        assert_eq!(error[0]["code"], "yaml.functions.referenced", "{error}");
        let message = error[0]["message"].as_str().unwrap();
        for location in [
            "函数“账号日常” → 第 3 步（automations/_function.yaml）",
            "第 4 步 → 条件成立 → 第 1 步",
            "第 4 步 → 否则 → 第 1 步 → 循环体 → 第 1 步",
            "第 5 步 → 模板分支 1 → 第 1 步",
            "第 5 步 → 未匹配 → 第 1 步",
            "函数“另一个函数” → 第 1 步（automations/_function_extra.yaml）",
            "自动化“每日自动化” → 第 1 步（automations/daily.yaml）",
        ] {
            assert!(message.contains(location), "missing {location}: {message}");
        }
        assert_eq!(message.matches("“账号登录” ←").count(), 7);
    }
    store
        .delete_resource("qa", YAML_EXTENSION_ID, "automations/_function_extra.yaml")
        .unwrap();
    store
        .delete_resource("qa", YAML_EXTENSION_ID, "automations/daily.yaml")
        .unwrap();
    let fixed = remaining.replace("账号登录: {}", "log: removed");
    assert!(validate_function_library_file(&store, "qa", file, &fixed).is_ok());
    assert_eq!(
        store
            .read_text("qa", YAML_EXTENSION_ID, file)
            .unwrap()
            .unwrap()
            .content,
        original
    );
}
