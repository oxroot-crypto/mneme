#![no_main]
//! WAL 回放模糊测试(I7):任意帧字节流回放不 panic、不无限循环。

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    mneme::fuzzing::replay_wal(data);
});
