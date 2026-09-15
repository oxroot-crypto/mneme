//! `Namespace` 写路径(`namespace/write/`)。
//!
//! 按动词拆分子模块:新增(`insert`)、删除(`delete`)、局部更新(`update`)与
//! 信念修订(`supersede`);各子模块均以 `impl Namespace` 扩展方法。

mod delete;
mod insert;
mod supersede;
mod update;
