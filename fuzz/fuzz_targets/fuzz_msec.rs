#![no_main]
//! `msec` 解码器模糊测试(I7/I18):任意字节输入不 panic、失败返回结构化错误。

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = mneme::fuzzing::parse_msec(data);
    mneme::fuzzing::version_injection(data);
});
