# OfferFlow 独立发行与更新状态

当前仓库为 [Ye-feng0510/OfferFlow](https://github.com/Ye-feng0510/OfferFlow)。**独立仓库暂未发布安装包，也未配置自己的更新签名体系。** 上游的历史发布记录、脚本和版本号不代表本仓库已发布对应版本。

## 当前行为

- `.github/workflows/release.yml` 仅支持 `workflow_dispatch`，只输出停用说明。
- 推送普通提交或 `v*` 标签都不会触发自动发版。
- 手动运行该工作流也不会构建、签名、上传、创建草稿或发布 Release。
- 工作流只有 `contents: read` 权限，不引用任何签名私钥或发布 secrets。
- CI 与 GitHub Pages 的目标分支仍为 `master`；网站部署不是桌面安装包发行。

## 客户端更新已停用

`src/lib/updater.tsx` 保留现有组件及 Hook API，方便已有调用方继续使用：

- 启动时不发起更新请求。
- `checkForUpdates()` 返回 `null`，不访问任何更新服务。
- `checkForUpdates(true)` 只显示“独立发行版本暂未启用自动更新”，不声称当前已是最新版本。
- `install()` 不下载、不安装、不重启。
- 更新弹窗不会显示。

`src-tauri/tauri.conf.json` 的应用标识为 `io.github.ye-feng0510.offerflow`，`productName` 为 `OfferFlow`，`bundle.createUpdaterArtifacts` 为 `false`。上游的 `plugins.updater` 地址和公钥已移除，不会转接上游更新渠道。

## 现在如何使用

按仓库 README 准备依赖后，本地执行：

```bash
pnpm tauri dev
# 或仅在本机构建
pnpm tauri build
```

本地构建不会发布 GitHub Release，也不会生成自动更新签名产物。现有版本号 `0.1.22` 是代码版本，不是本独立仓库的发行承诺。

## 将来启用独立发行前的必要条件

以下是未来工作，不是已完成的配置：

1. 由仓库维护者生成并安全备份自己持有的更新签名密钥；不得复用上游或历史工作目录中的私钥。
2. 在明确授权后配置仓库 secrets；私钥不进入 Git、日志或发布附件。
3. 配置与独立私钥配对的客户端公钥，以及仅属于本仓库的更新地址。
4. 重新审查并启用构建、签名、元数据、草稿聚合和发布工作流，确保任何失败均不公开不完整版本。
5. 同步 `package.json`、`src-tauri/Cargo.toml`、`src-tauri/tauri.conf.json` 及必要的锁文件版本，验证标签与代码一致。
6. 在目标平台实际验证安装、升级、验签失败保护及数据目录隔离，再恢复客户端检查与安装能力。

Tauri 更新签名不等同于 Windows Authenticode 或 Apple Developer ID 签名/公证；这些平台信任机制仍需另行配置。
