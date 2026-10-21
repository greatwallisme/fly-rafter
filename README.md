# Fly Rafter（飞椽）

以 [dougong](https://crates.io/crates/dougong) 组合内核为基座的 AI agent
harness 系统，行为与协议 1:1 对标 deepseek-harness。全部组件均为插件；
宿主二进制 `fr` 只承担声明式装配、事件泵与入口分发。

- 需求真源：`doc/prd.md`
- 合约体系：`doc/requirements-intake.md` → `knowledge/requirements/` → `specs/generated/`
- 里程碑：headless → TUI → ACP → SDK → web（PRD 第 20 节）

## 质量门

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```
