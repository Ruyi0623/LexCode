//! 仓库根两份项目指引必须逐字节一致:`AGENTS.md` 与 `CLAUDE.md`。
//!
//! 为什么必须一致(而不是「最好一致」):
//! - `AGENTS.md` 会被 lex-code 自身在启动时读取并注入**产品自己的系统提示词**
//!   (`lex-core/src/context/agents_md.rs`,读 `<cwd>/AGENTS.md`),直接改变模型行为;
//! - `CLAUDE.md` 是 Claude Code 读取的那一份,驱动的是开发期的编码助手。
//!
//! 两份文件刻意保持同源同内容的副本(不用符号链接:那会破坏 Windows 检出)。
//! 若只改其中一份,产品的运行时行为会与开发期看到的内容静默漂移,而没有任何其他机制会发现。

use std::path::{Path, PathBuf};

fn repo_root_file(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(name)
}

#[test]
fn agents_md_and_claude_md_are_byte_identical() {
    let agents = repo_root_file("AGENTS.md");
    let claude = repo_root_file("CLAUDE.md");
    let a = std::fs::read(&agents).unwrap_or_else(|e| panic!("读取 {} 失败: {e}", agents.display()));
    let c = std::fs::read(&claude).unwrap_or_else(|e| panic!("读取 {} 失败: {e}", claude.display()));
    assert_eq!(
        a,
        c,
        "AGENTS.md 与 CLAUDE.md 必须逐字节一致:AGENTS.md 会在运行时被注入 lex-code 自身的系统提示词\
         (lex-core/src/context/agents_md.rs),CLAUDE.md 则是 Claude Code 读取的同一份内容。\
         只改一份会让模型行为与开发期看到的内容静默漂移 —— 请把改动同时同步到两份(长度:AGENTS.md={} 字节, CLAUDE.md={} 字节)",
        a.len(),
        c.len()
    );
}
