# 钉钉文档（DingTalk Docs）

quiet.md 是本地优先的笔记应用，没有云端账号，也没有钉钉 OpenAPI 登录。和飞书一样，对接外部文档靠的是**文件导入 / 导出**和**剪贴板粘贴**，不是后台同步。

钉钉文档（alidocs / adoc）没有飞书那种 DocxXML；官方互通格式是 **Markdown**（也支持导出 Word / PDF）。因此 quiet.md 选择：

| 方向 | 做法 |
|---|---|
| 从钉钉文档进来 | 导出 `.md` 后「导入」；或直接复制粘贴 HTML |
| 从 quiet.md 出去 | 「下载为 → Markdown」，再在钉钉文档中导入 / 粘贴 |
| 云端实时同步 | **不做**（需要企业应用凭证；见文末 Follow-ups） |

## 从钉钉文档导入

### 方式一：导出 Markdown（推荐）

1. 在电脑端打开钉钉文档。
2. 右上角 **┇ → 下载到本地 → Markdown（.md）**。  
   （找不到工具栏时：偏好设置 → 界面偏好 → 顶部工具栏。）
3. 在 quiet.md 里：**导入 / 上传**，选中该 `.md` 文件。

导入时会识别钉钉文档 Markdown 里常见的：

- `::: … :::` / 多行 `:::` 围栏 → 高亮块（callout）
- `==文字==` → 浅黄背景高亮
- 标题、列表、待办、引用、表格、代码块、链接、图片（与通用 Markdown 相同）

钉钉独有的卡片、脑图、投票、附件等会在导出时被钉钉降级成提示文字或链接；quiet.md 按普通 Markdown 读入，不会假装还原那些块。

### 方式二：复制粘贴

1. 在钉钉文档中选中内容，复制。
2. 在 quiet.md 笔记里粘贴。

钉钉剪贴板 HTML 常带 `data-clipboard-cangjie`，并把结构包在 `<template>` 里。quiet.md 会解开 `<template>`、保留粗体 / 列表等结构，且不会再对这份 HTML 做一遍 Markdown 重解析（与飞书 `lark-doc` 粘贴同类处理）。

### 方式三：导入 HTML 文件

「导入」也接受 `.html` / `.htm`（例如自己保存的剪贴板 HTML）。转换规则与粘贴相同。

## 导出到钉钉文档

1. 在 quiet.md：**下载为 → Markdown**（需要图片时选「所有内容」）。
2. 在钉钉文档中新建或打开文档，把 Markdown 贴进去，或用钉钉侧「导入」/ OpenAPI / CLI（如 `dws doc +import`）上传 `.md`。

quiet.md **不**内置钉钉登录，也不会替你调用 `api.dingtalk.com`。

## 不支持 / 已知限制

- **`.docx` Word 导出**：钉钉可下载为 Word；quiet.md 目前不导入 `.docx`。请改用 Markdown，或先在钉钉导出 `.md`。
- **表格 / 脑图 / 白板 / 应用表格** 等非文字文档：不在本文范围。
- **图片**：Markdown 里的相对路径会按文件旁目录解析；远程 URL 按设置决定是否加载。钉钉签名 URL 有时效，过期后需重新导出。
- **OpenAPI 双向同步**：未实现（见下）。

## Follow-ups（需要真实钉钉凭证）

若以后要做在线拉取 / 推送，需要用户或企业在[钉钉开放平台](https://open.dingtalk.com/)创建应用，并自行保管 `AppKey` / `AppSecret` / 访问令牌。可能的方向：

1. 用文档 OpenAPI 把 adoc 读成 Markdown，再走现有 `markdown_to_blocks`。
2. 用覆写 / 插入接口把 quiet.md 导出的 Markdown 写回钉钉。

这些都改变「本地优先、默认不联网」的产品边界，因此本版本只做到文件与粘贴层，与飞书 DocxXML / Markdown 的集成方式对齐。
