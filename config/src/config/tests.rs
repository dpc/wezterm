use super::*;

#[test]
fn lua_file_export_policy_defaults_overrides_and_invalid_values() {
    let lua = mlua::Lua::new();
    let load = |source: &str| {
        let value: mlua::Value = lua.load(source).eval().unwrap();
        Config::from_dynamic(
            &luahelper::lua_value_to_dynamic(value).unwrap(),
            Default::default(),
        )
    };
    let default = load("return {}").unwrap();
    assert_eq!(default.osc5522_file_export_limit, 16 * 1024 * 1024);
    let raised = load("return {osc5522_file_export_limit = 128 * 1024 * 1024}").unwrap();
    assert_eq!(raised.osc5522_file_export_limit, 128 * 1024 * 1024);
    assert_eq!(
        raised.osc5522_clipboard_read_limit,
        default.osc5522_clipboard_read_limit
    );
    assert!(!raised.allow_osc5522_clipboard_write);
    assert_eq!(
        load("return {osc5522_file_export_limit = 0}")
            .unwrap()
            .osc5522_file_export_limit,
        0
    );
    for value in ["-1", "1.5", "'128MiB'"] {
        assert!(load(&format!("return {{osc5522_file_export_limit = {value}}}")).is_err());
    }
    let too_large = isize::MAX as usize - wezterm_term::clipboard::FILE_EXPORT_OVERHEAD + 1;
    assert!(load(&format!(
        "return {{osc5522_file_export_limit = {too_large}}}"
    ))
    .is_err());
}
