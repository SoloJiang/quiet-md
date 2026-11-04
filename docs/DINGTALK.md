# 钉钉文档（DingTalk Docs）

quiet.md 是本地优先的笔记应用。对接钉钉文档有两层：

1. **文件 / 粘贴**（默认可用，不联网）：Markdown / HTML 导入，以及钉钉剪贴板 HTML。
2. **OpenAPI**（可选）：用**用户授权（OAuth）**拉取 / 覆写 / 追加 / 新建钉钉文档。

默认不联网。配置 OAuth 应用凭证后，菜单里「连接钉钉账号」完成用户登录；之后读写使用用户 `accessToken`，而不是企业内部应用的 `gettoken` 应用级 token。

## 原则

| 项 | 选择 |
| --- | --- |
| 授权方式 | **用户授权**（系统浏览器 + 本机 loopback 回调） |
| AppKey / AppSecret | 仅作 OAuth `clientId` / `clientSecret` |
| 互通格式 | **Markdown**（与钉钉覆写 / 插入接口一致） |
| 桌面回调默认 | `http://127.0.0.1:17865/callback` |
| Token 存放 | `~/Documents/Quiet.md/dingtalk-oauth.json`（或 `$QUIET_MD_HOME/dingtalk-oauth.json`） |

## 控制台配置

### 1. 钉钉开放平台

1. 打开 [钉钉开放平台](https://open.dingtalk.com/) → 创建**企业内部应用**（或具备登录能力的应用）。
2. 记下 **AppKey**（OAuth `clientId`）和 **AppSecret**（`clientSecret`）。
3. 在应用里开通文档相关权限（读块 / 写内容等，以控制台当前权限名为准），并发布。
4. 配置 **登录 / OAuth 回调地址（redirect_uri）**，必须与客户端一致。桌面默认：

   ```
   http://127.0.0.1:17865/callback
   ```

   若改端口或路径，同步设置环境变量 `QUIET_MD_DINGTALK_REDIRECT_URI`。
5. 登录 scope 默认使用 `openid corpid`（可按控制台要求调整；代码里写在 `DEFAULT_SCOPE`）。

### 2. 环境变量

```sh
export QUIET_MD_DINGTALK_APP_KEY='你的 AppKey'          # OAuth clientId
export QUIET_MD_DINGTALK_APP_SECRET='你的 AppSecret'    # OAuth clientSecret
# 可选：覆盖默认本机回调
# export QUIET_MD_DINGTALK_REDIRECT_URI='http://127.0.0.1:17865/callback'
# 新建文档时需要知识库 / 工作区 id
# export QUIET_MD_DINGTALK_WORKSPACE_ID='……'
# 默认目标文档（URL 或 dentryUuid）；未设则用剪贴板里的链接
# export QUIET_MD_DINGTALK_DOC='https://alidocs.dingtalk.com/i/nodes/……'
# 测试 / 无浏览器：直接塞入授权码（跳过打开浏览器）
# export QUIET_MD_DINGTALK_AUTH_CODE='……'
```

`OPERATOR_ID` **不再需要**：连接成功后从 `/v1.0/contact/users/me` 读取 `unionId`，并写入本地会话。

## 用户怎么连

1. 设好 `APP_KEY` / `APP_SECRET`（以及控制台里的 redirect URI）。
2. 打开一篇笔记 → 右上角 **… → 钉钉文档 → 连接钉钉账号**。
3. 系统浏览器打开钉钉授权页；同意后浏览器跳到本机 `127.0.0.1:17865/callback`。
4. quiet.md 用 `authCode` 换取 `userAccessToken` / `refreshToken`，拉取用户 `unionId`，写入 `dingtalk-oauth.json`。
5. 之后「拉取 / 推送 / 追加 / 新建」都带用户 token；过期时用 `refreshToken` 自动刷新。
6. **断开钉钉账号** 会删除本地会话文件（不调用远程登出）。

### OAuth 步骤（实现对应）

| 步骤 | 端点 / 行为 |
| --- | --- |
| 授权 | `https://login.dingtalk.com/oauth2/auth?redirect_uri=…&response_type=code&client_id=…&scope=openid%20corpid&prompt=consent&state=…` |
| 回调 | 本机 HTTP：解析 `authCode`（或 `code`）与 `state` |
| 换 token | `POST https://api.dingtalk.com/v1.0/oauth2/userAccessToken`（`grantType=authorization_code`） |
| 刷新 | 同上，`grantType=refresh_token` + `refreshToken` |
| 用户信息 | `GET /v1.0/contact/users/me`（header `x-acs-dingtalk-access-token`） |
| 读块 | `GET /v1.0/doc/suites/documents/{docKey}/blocks?operatorId={unionId}&…` |
| 覆写 | `POST /v2.0/doc/me/suites/documents/{docKey}/overwriteContent`（用户委托路径） |
| 追加 | `POST /v1.0/doc/suites/documents/{docKey}/content?operatorId={unionId}` |
| 新建 | `POST /v1.0/doc/workspaces/{workspaceId}/docs`（需 `WORKSPACE_ID`） |

## 菜单

打开一篇笔记 → **… → 钉钉文档**：

| 项 | 作用 |
| --- | --- |
| **连接钉钉账号** / **断开钉钉账号** | 用户 OAuth 登录或清除本地会话 |
| **拉取到本地** | OpenAPI 拉块 → Markdown → 新建本地笔记 |
| **推送（覆写）** | 当前笔记 Markdown **清空并覆写**目标钉钉文档（≤ 50000 字符） |
| **追加到钉钉** | 当前笔记 Markdown **追加**到目标文档末尾 |
| **新建并推送** | 在 `WORKSPACE_ID` 下新建文档并写入内容 |

目标文档解析顺序：

1. `QUIET_MD_DINGTALK_DOC`
2. 否则剪贴板里的钉钉文档链接（`https://alidocs.dingtalk.com/i/nodes/…`）

## 不联网：文件与粘贴

### Markdown 文件

1. 钉钉文档：右上角 **┇ → 下载到本地 → Markdown（.md）**。
2. quiet.md：**导入**，或把文件拖进来。

识别钉钉 Markdown 常见写法：`::: … :::` 高亮块、`==文字==` 高亮等。

### HTML / 剪贴板

钉钉剪贴板常带 `data-clipboard-cangjie`，内容包在 `<template>` 里。quiet.md 会解开 template 并保留结构（与飞书 `lark-doc` 同类处理）。

## 注意

- **覆写**会清空钉钉侧原有内容（含评论等），请先确认目标。
- 用户 token 存在本机文档库目录；勿把 `dingtalk-oauth.json` 提交进 git。
- 企业若关闭了个人授权或未给应用开通文档权限，连接或读写会失败——以控制台与组织策略为准。
- 块结构 → Markdown 是 best-effort；复杂表格 / 嵌入可能损失格式。

库代码：`crates/core/src/dingtalk.rs`（`fdl_core::dingtalk`）。
