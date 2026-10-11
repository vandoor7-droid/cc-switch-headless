# 同步上游（farion1231/cc-switch）

这个仓库是 [farion1231/cc-switch](https://github.com/farion1231/cc-switch) 的 fork，
上游会持续加新供应商预设、新 CLI 支持、修代理和会话解析的 bug。这份文档是怎么把它们
合进来、以及合并时要注意什么。

基线记录在 [`upstream-v4.0.0.lock`](upstream-v4.0.0.lock)：那个文件里的
`last_merged_commit` 就是「这个 fork 跟到上游哪里」。

## 一次性配置

```bash
git remote add upstream https://github.com/farion1231/cc-switch.git
git fetch upstream --tags
```

`--tags` 是安全的：本 fork 的 tag 叫 `v4.0.0-headless` / `.1` / `.2`，和上游的 `v4.0.0`
不会撞名（这也是当初特意取这个前缀的原因）。

## 每次同步

```bash
git checkout main
git pull                                          # 先把自己 fork 的 main 更新到最新

git checkout -b sync/upstream-2026-11             # 开同步分支，别直接在 main 上解冲突
git fetch upstream
git log --oneline HEAD..upstream/main | head -50  # 先看上游都改了什么
git diff --stat HEAD...upstream/main -- src-tauri src   # 看看改动规模

git merge upstream/main
# 解冲突 → git commit → git push -u origin sync/upstream-2026-11
# 然后开 PR，等 CI 全绿，再合回 main
```

**走 PR，不要直接推 main。** 合并后最容易出问题的地方是编译（见下面「合并后必须验证」），
而 CI 里的 `Server Checks (headless)` 正是为这件事存在的——它会在 PR 上把 server feature
编一遍并跑完整 e2e。

## 为什么用 merge，不用 rebase

- `main` 已经推到公开仓库，rebase 会重写历史、必须 force-push，会打断任何 clone 过它的人；
- 本 fork 改了 **50+ 个上游已有的文件**（要让 crate 在 `server` feature 下编译，很多共享文件
  被加了 `#[cfg(feature = "desktop")]` 门），rebase 意味着**每次都要重新解一遍**这些冲突，
  merge 只解一次；
- 历史本来就是 merge 出来的（`git log --merges`）。

## 实测：一次合并要解多少冲突

2026-10-11 干跑过一次（`git merge --no-commit --no-ff upstream/main`，上游停在 **v4.0.8**，
本 fork 落后 147 个提交、上游侧改了 479 个文件）：

**只有 10 个文件冲突、合计 15 处**，其余 469 个文件自动合并。

| 冲突处数 | 文件 |
| --- | --- |
| 3 | `.github/workflows/release.yml` |
| 2 | `src-tauri/src/proxy/forwarder.rs` |
| 2 | `src-tauri/Cargo.toml` |
| 2 | `README.md` |
| 1 | `.github/workflows/ci.yml` |
| 1 | `package.json` |
| 1 | `src-tauri/src/lib.rs` |
| 1 | `src-tauri/src/commands/import_export.rs` |
| 1 | `src-tauri/src/services/provider/codex_official_models.rs` |
| 1 | `src/App.tsx` |

都是「两边都改了同一处」的机械冲突（fork 的 `#[cfg]` 门 / workflow 触发条件 / 文档与版本号），
没有需要重新设计的。**想看当前会冲突什么，自己干跑一次即可**：

```bash
git fetch upstream
git merge --no-commit --no-ff upstream/main   # 在同步分支上
git diff --name-only --diff-filter=U          # 冲突文件
git merge --abort                             # 看完撤销
```

## 冲突高发区

| 类别 | 文件 | 说明 |
| --- | --- | --- |
| feature 门 | `src-tauri/src/lib.rs`、`commands/*`、`services/*`、`proxy/*` | 为 `server` feature 加的 `#[cfg(feature = "desktop")]`；上游一动这些文件就会冲突 |
| 构建配置 | `src-tauri/Cargo.toml`、`build.rs` | `desktop`/`server` feature 声明、`dist-web` 占位逻辑 |
| 文档与流水线 | `README*`、`.github/workflows/*` | fork 的发布面（`release-server.yml` 等） |
| **基本不会冲突** | `src-tauri/src/web/**`、`src/web-shims/**`、`scripts/e2e/**`、`docs/web-mode-zh.md` | fork 自己的新文件，上游不会碰 |

唯一要动脑的情况：**上游改了 `web/` 用到的 service 函数签名**。这时改 `web/routes.rs` 那层
参数解析即可——业务逻辑在 `services/` 里只有一份，不用重复实现。

## 合并后必须验证

顺序与 CI 的 `server` job 一致：

```bash
pnpm build:web
cargo clippy --manifest-path src-tauri/Cargo.toml --no-default-features --features server --bin cc-switch-server -- -D warnings
cargo build  --manifest-path src-tauri/Cargo.toml --no-default-features --features server --bin cc-switch-server
bash scripts/e2e/run-all.sh
```

（`CONTRIBUTING.md` 里也列了这几条。）

## 合并完更新基线

把 `docs/upstream-v4.0.0.lock` 改名成新的基线并更新内容，例如合到上游 `v4.1.0` 之后：

```bash
git mv docs/upstream-v4.0.0.lock docs/upstream-v4.1.0.lock
# 改 upstream_tag / upstream_commit / last_merged_commit / resolved_at
```

`upstream_commit` 取 **tag peel 后的 commit**（`git ls-remote upstream 'refs/tags/v4.1.0*'`
能看到 tag 对象和它指向的 commit），`last_merged_commit` 填这次 merge 进来的上游最新提交。

## 发版

同步上游**不会**自动发版。要让新特性进到下载的产物里，还得打一个新 tag（`v*`），
`release-server.yml` 会重新构建三平台二进制、在各自的 runner 上跑一遍 smoke test，
再挂到 Release。
