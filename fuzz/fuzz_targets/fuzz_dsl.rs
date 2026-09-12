#![no_main]
//! 过滤 DSL 解析模糊测试(I7):任意输入不 panic、语法错误返回结构化错误。

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = mneme::fuzzing::parse_dsl(data);
});
