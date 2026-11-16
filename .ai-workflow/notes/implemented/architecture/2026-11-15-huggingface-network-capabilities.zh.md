# Agent Note: HuggingFace 下载、网络传输与 Cookie 来源

Status: implemented

[English](2026-11-15-huggingface-network-capabilities.md) | 中文

## Problem

OneGet 可以下载普通 HTTP 文件和视频，但没有 HuggingFace 解析器、没有全局代理选择，只有位于只读网络占位界面之后的每次下载 Cookie 和代理输入。需要下载需要授权的 HuggingFace 文件、使用公司系统代理或 SOCKS5 代理、面对缓慢源站或使用浏览器会话的用户只能手动绕过。若没有记录在案的决策，后续工作可能把访问令牌附加到 CDN 主机、把浏览器 Cookie 持久化到任务存储、合并 Cookie 来源、把 PAC 或 WPAD 配置当作可用代理，或以破坏旧版桌面端或扩展的方式改变捕获协议。

## Decision

HuggingFace 识别仅限 `huggingface.co`、`www.huggingface.co`、`hf.co` 和 `www.hf.co`，以及模型、`/datasets/` 和 `/spaces/` 下 `/resolve/` 或 `/blob/` 的文件路径。`/blob/` 路径会规范化为 `/resolve/`，其他解析器路径在创建任务前返回 `HF_NOT_A_FILE`，CDN 或无关主机会作为普通 URL 处理，因此令牌绝不会附加到它们。解析器返回签名 CDN URL；访问令牌只发送给解析器主机请求，第一个离开解析器主机的重定向会被直接返回，而不联系 CDN。

首次解析在收到 401 后可以重试一次，该重试不属于签名刷新；解析器重定向最多跟随十次。首次解析成功后，CDN 403 最多可触发三次签名刷新，因此完全触发时一个任务最多联系解析器四次。没有可用授权的 401 返回 `HF_AUTH`；过期的 CDN 签名会在不重新下载已完成字节的情况下刷新，刷新计数从 0 开始并在达到上限时恰好为 3，耗尽后返回 `HF_SIGNATURE`，而报告未授权的刷新返回 `HF_AUTH`。保存的连接上限为 1 到 64，默认 8，在任务启动时限制解析器任务；429 或被忽略的 Range 会让任务在本次运行的剩余时间里降为单连接；`hf.db` schema 版本 1 保存仓库、文件、修订、上限、生效连接数、刷新次数和降级状态，因此暂停的任务可以在重启后通过重新解析其已存解析器 URL 继续。

代理选择有三种模式。`direct` 是默认值，忽略已存凭据和系统配置。`system` 读取 macOS `scutil --proxy` 或 Windows Internet Settings 值，优先 HTTPS 端点再 HTTP，且从不携带凭据；PAC、WPAD、自动检测或不可读配置会报告稳定原因并直接连接。`manual` 支持 `http`、`https` 和 `socks5`，要求非空主机、1 到 65535 的端口，以及位于操作系统凭据存储中的可选基础认证凭据。代理 URL 绝不内嵌 userinfo，保存 direct 或 system 模式时会删除凭据，生效代理应用于 HTTP 引擎、视频服务和组件下载器。

连接和读取超时可配置，默认分别为 15 秒和 30 秒；重试次数可配置为 1 到 10，默认 5；没有每次任务值时，配置的非空且不含换行符的 User-Agent 作为 HTTP 和 HuggingFace 请求的后备值。这些选项应用于保存后启动或继续的任务；已在运行的子进程保留其启动时捕获的代理和传输配置。

一个任务只有一个 Cookie 来源，绝不合并列表。每次任务或默认 Netscape Cookie 文件优先，然后是捕获的浏览器 Cookie。Cookie 文件按主机、包含子域、路径、secure 和过期时间匹配，在每次启动或继续时重新加载，并在每个重定向跳转时重新计算。捕获的浏览器 Cookie 仅存在于内存，每个条目最多 100 个、合计最多 8 KiB，在捕获和使用时按 URL 限定范围，并通过不透明句柄单次使用；未知或已消费的句柄返回 `INVALID_COOKIE_HANDLE`，范围不匹配返回 `COOKIE_SCOPE`，超出限制的列表返回 `COOKIE_LIMIT`。

捕获协议版本 2 增加了该可选 Cookie 列表，而桌面端仍接受不带 Cookie 的版本 1 捕获。扩展在两个包中都请求 `cookies` 权限，但仅在默认关闭的 `sendCookies` 设置启用时读取并附加 Cookie，且只附加按捕获 URL 限定范围的 Cookie。Cookie 值绝不进入设置、任务数据库、HuggingFace 数据库或日志，MAIN-world 观察器绝不读取 Cookie。

设置保持 schema 版本 2 且只增加可选字段，因此没有本变更的构建会加载默认值、保留未知字段并继续工作。任务存储保持 schema 版本 3，`hf.db` 是独立的 schema 版本 1 文件，代理凭据和 HuggingFace 令牌保存在凭据存储中。回滚后设置仍可读、任务仍可列出，`hf.db` 保留为未使用元数据；回滚扩展包会把捕获行为恢复到版本 1；旧版桌面端会以 `PROTOCOL_MISMATCH` 拒绝版本 2 捕获。

本记录部分取代 [下载引擎与持久任务存储](2026-11-12-download-engine-task-store.md)、[桌面下载界面与引擎服务](2026-11-13-desktop-download-ui.md)、[签名组件与视频流水线](2026-11-13-component-video-pipeline.md)、[回环浏览器捕获与浏览器扩展集成](2026-11-13-local-api-browser-capture.md) 和 [通过共享控制器实现 Firefox 扩展对等](2026-11-13-firefox-extension-parity.md) 记录中的网络表述。这些记录在它们仍拥有的引擎、存储、界面、视频、配对、捕获和 Firefox 决策上保持有效，并且每一份都交叉链接到本记录。

## Alternatives considered

- 把 HuggingFace 令牌发送给 CDN 或每个主机被否决，因为签名 CDN URL 不需要令牌，而更宽的令牌范围会把长期凭据泄露给无关基础设施。
- 只保留每次下载的代理和 Cookie 输入被否决，因为系统代理和公司代理、共享默认 Cookie 文件以及浏览器会话是日常需求，每次任务字段无法表达。
- 把浏览器 Cookie 或代理凭据存入设置或任务数据库被否决，因为这些值具有浏览器或操作系统生命周期；凭据存储和仅内存句柄让它们远离持久产品数据。
- 合并 Cookie 文件和捕获的浏览器 Cookie 被否决，因为两个来源可能在域、路径和过期时间上不一致，单一明确优先级更易推理和测试。
- 通过执行来信任 PAC 或 WPAD 配置被否决，因为那会增加脚本引擎和远程发现依赖；明确的不可用原因和直接连接是更小、更可审计的边界。
- 在每次捕获时无选项地读取浏览器 Cookie 被否决，因为那会扩大扩展权限的影响并让用户意外；默认关闭的开关把 Cookie 访问限制为明确选择。
- 把 HuggingFace 元数据存入任务数据库被否决，因为这些详情是建议性的，缺失或损坏的元数据文件绝不能影响任务列出或控制；独立的 `hf.db` 让该故障保持独立。
- 在新设置 schema 版本下增加新字段或改写任务存储被否决，因为可选字段和独立元数据文件让旧读取器继续工作，并使回滚不具破坏性。

## Consequences

后续网络工作必须保留解析器主机令牌边界、签名 URL 刷新与连接上限、单一来源 Cookie 优先级、仅内存捕获 Cookie 句柄、带有稳定 PAC 与不可读原因的三种代理模式，以及可配置传输选项。扩展 Cookie 权限只能在默认关闭的选项之后使用，且只能用于按 URL 限定范围的 Cookie；增加另一个捕获字段或协议版本需要兼容的增量变更和同样的凭据排除。

交付的验证使用本地 HuggingFace、代理、SOCKS5、系统代理和 Cookie 夹具，因此不能证明公共 HuggingFace 主机、真实公司代理、真实浏览器 Cookie 填充或 Windows 行为。这些项目仍是手动且待完成，自动化通过结果不是阶段退出批准。

- 缺失或损坏的 `hf.db` 让任务仍可列出，只丢失 HuggingFace 详情；删除它不会移除任务、文件、设置或配对。
- 生效代理和传输设置应用于保存后启动或继续的任务，而运行中的子进程保留其启动时捕获的配置。
- 上下文索引器只发现 TypeScript 模块，因此 Rust 的 `engine`、`store`、`platform`、`commands`、`api`、`kernel`、`video` 和工具导航条目仍由手工维护。
