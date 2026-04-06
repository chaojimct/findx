//! FindX 原生紧凑索引：单块字符串池 + 扁平记录 + 外部哈希映射，避免 CLR 逐文件托管对象开销。

mod engine;
mod ffi;

pub use engine::Engine;
