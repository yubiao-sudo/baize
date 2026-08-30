# -*- coding: utf-8 -*-
"""lib.rs：注册浏览器路径命令 + 启动时恢复手动指定的浏览器路径"""

src = open("lib.rs", encoding="utf-8").read()

a = "            commands::browser_act,"
assert a in src, "browser_act 注册行未找到"
src = src.replace(
    a,
    "            commands::browser_act,\n            commands::browser_get_path,\n            commands::browser_set_path,",
    1,
)

b = """        // 数据库连接配置：从持久化恢复
        if let Ok(Some(json)) = state.store.get_setting("db_connections") {
            if let Ok(list) = serde_json::from_str::<Vec<tools::DbConnection>>(&json) {
                tools::refresh_db_connections(&list);
            }
        }
"""
add = b + """
        // 桌面浏览器路径：从持久化恢复（Chrome 发现的设置指定优先级最高）
        if let Ok(Some(p)) = state.store.get_setting("browser_chrome_path") {
            crate::browser::set_custom_browser_path(Some(&p));
        }
"""
assert b in src, "db_connections 恢复块未找到"
src = src.replace(b, add, 1)

open("lib.rs", "w", encoding="utf-8", newline="\n").write(src)
print("lib.rs 已更新")
