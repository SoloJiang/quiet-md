# 钉钉文档（DingTalk Docs）

quiet.md 是本地优先的笔记应用。对接钉钉文档有两层：

1. **文件 / 粘贴**（无需账号）：Markdown / HTML 导入、剪贴板粘贴。
2. **OpenAPI 拉取 / 推送**（可选）：用企业内部应用凭证，把 adoc 正文以 Markdown 形式进出处。

默认不联网；只有配置了下面的环境变量，菜单里的「钉钉文档」才会真正调用 `api.dingtalk.com`。

## 设计选择

| 选项 | 决定 |
|---|---|
| 鉴权 | **企业内部应用 AppKey / AppSecret**（应用授权 accessToken）。桌面端最省事：无需嵌浏览器做用户 OAuth。不支持第三方个人应用。 |
| 互通格式 | **Markdown**（与钉钉覆写 / 插入接口一致） |
| 拉取 | `GET …/documents/{docKey}/blocks` → 块转 Markdown → 现有 `markdown_to_blocks` |
| 推送 | `POST …/overwriteContent`（全文覆写）或 `POST …/content`（追加） |
| 新建 | `POST …/workspaces/{workspaceId}/docs`（`docType=DOC`）再覆写正文 |

## OpenAPI 配置

### 1. 钉钉开放平台控制台

1. 打开 [钉钉开放平台](https://open.dingtalk.com/) → 创建**企业内部应用**。
2. 记下 **Client ID（AppKey）** 和 **Client Secret（AppSecret）**。
3. **权限管理**中申请并开通至少：
   - 企业存储文件读权限（`Storage.File.Read`）— 查询块 / 拉取
   - 企业存储文件写权限（`Storage.File.Write`）— 覆写 / 追加
   - 知识库文档写权限（`Document.WorkspaceDocument.Write`）— 新建（若要用「新建并推送」）
4. 发布应用，并按组织要求完成管理员审批。
5. 用[查询用户详情](https://open.dingtalk.com/document/orgapp/query-user-details)拿到操作者的 **unionId**（接口里的 `operatorId`）。
6. （可选）用知识库列表接口拿到 **workspaceId**，供「新建并推送」使用。

### 2. 环境变量

启动 quiet.md 前设置（不要把 Secret 写进仓库或文档库）：

```bash
export QUIET_MD_DINGTALK_APP_KEY='dingxxxxxxxx'          # Client ID
export QUIET_MD_DINGTALK_APP_SECRET='GT-xxxxxxxxxxxx'    # Client Secret
export QUIET_MD_DINGTALK_OPERATOR_ID='你的unionId'
# 可选：
export QUIET_MD_DINGTALK_WORKSPACE_ID='知识库workspaceId'  # 新建文档时需要
export QUIET_MD_DINGTALK_DOC='https://alidocs.dingtalk.com/i/nodes/……'  # 默认目标文档
```

macOS 可用 `launchctl setenv` 或在终端里先 export 再 `scripts/preview.sh`。

### 3. 在 quiet.md 里用

打开一篇笔记 → 右上角 **… → 钉钉文档**：

| 菜单 | 行为 |
|---|---|
| **拉取到本地** | 从目标文档拉 Markdown，新建一篇本地笔记并打开 |
| **推送（覆写）** | 把当前笔记导出为 Markdown，**清空并覆写**目标钉钉文档（≤ 50000 字符） |
| **追加到钉钉** | 把当前笔记 Markdown **追加**到目标文档末尾 |
| **新建并推送** | 在 `WORKSPACE_ID` 知识库新建 DOC，再写入当前笔记 Markdown |

**目标文档**解析顺序：

1. `QUIET_MD_DINGTALK_DOC`（URL 或 dentryUuid / docKey）
2. 否则用剪贴板里的钉钉文档链接（`https://alidocs.dingtalk.com/i/nodes/…`）

未配置凭证或找不到目标时，会 toast 提示，不会静默联网。

## 文件 / 粘贴（无需 OpenAPI）

### 导出 Markdown 后导入

1. 钉钉文档：右上角 **┇ → 下载到本地 → Markdown（.md）**。
2. quiet.md：**导入 / 上传** 该 `.md`。

识别钉钉 Markdown 常见写法：`::: … :::` 高亮块、`==文字==` 高亮等。

### 复制粘贴

钉钉剪贴板常带 `data-clipboard-cangjie`，内容包在 `<template>` 里。quiet.md 会解开 template 并保留结构（与飞书 `lark-doc` 同类处理）。

### HTML 文件

「导入」也接受 `.html` / `.htm`。

## 已知限制 / Follow-ups

- **仅 ALIDOC 文字文档**；表格、脑图、白板等不在范围。
- **拉取**靠一级块查询再转 Markdown，嵌套块 / 复杂表可能降级；图片多为远程 URL。
- **覆写**会清空钉钉侧原有内容（含评论等），请先确认目标。
- **accessToken** 缓存在进程内约 2 小时；换凭证需重启应用。
- **用户 OAuth / 个人授权覆写**未实现（桌面端优先应用凭证）。
- 无真实凭证时，OpenAPI 路径靠 mock HTTP 单测覆盖；联调需你在控制台配好应用。

库代码：`crates/core/src/dingtalk.rs`（`fdl_core::dingtalk`）。
