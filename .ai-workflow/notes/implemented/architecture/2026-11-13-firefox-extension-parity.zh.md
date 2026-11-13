# Agent Note: 通过共享控制器实现 Firefox 扩展对等

Status: implemented

[English](2026-11-13-firefox-extension-parity.md) | 中文

## Problem

浏览器能力此前只面向 Chromium 交付：回环 API 只接受 `chrome-extension://` 来源，控制器、内容脚本、观察器和接管触发都位于 Chromium 包内。Firefox 提供同样的捕获与预填充能力，但在决定正确性的细节上不同：后台是可被挂起的事件页，下载 API 使用 promise，`downloads.onCreated` 取代 `onDeterminingFilename`，绝对下载文件名必须变成安全的基本名，`totalBytes: -1` 表示长度未知，其扩展来源是 `moz-extension://` UUID。在不复制决策逻辑、不削弱现有 API、配对和捕获边界的前提下扩展该能力，需要自己的记录；[回环浏览器捕获记录](2026-11-13-local-api-browser-capture.md) 仍拥有它继续描述的配对、捕获、预填充和隐私决策。

## Decision

共享包拥有与浏览器无关的行为。`extension/shared/background.ts` 导出一个控制器，负责配对、观察、接管、预填充、资源边界和失败状态；`extension/shared/browser.ts` 是一个小型类型化适配器，统一回调风格的 Chromium 与 promise 风格的 Firefox 的消息、活动标签页查询、本地与会话存储以及响应头。隔离内容脚本、MAIN-world 观察器和覆盖层样式移入 `extension/shared`。`extension/chromium/background.ts` 变成薄安装器，注册现有 service worker 监听器、注入 Chrome 依赖，并保留 Chromium 独有的 `onDeterminingFilename` 流程；`extension/firefox/background.ts` 是事件页适配器，同步注册 runtime、响应、下载、导航和标签页监听器，注入基于 promise 的 `browser.*` API，并以 `Firefox extension` 作为配对标签。弹窗导入共享表面，共享控制器通过依赖接收配对标签，而不改变消息协议。

Firefox 包是独立的 MV3 产物，使用 `background.scripts` 而非 service worker，稳定 Gecko ID 为 `oneget@oneget.app`，`strict_min_version` 为 `140.0`，并声明 `data_collection_permissions.required: ["none"]`。它请求与 Chromium 相同的五项权限、相同的 HTTP(S) 与回环主机权限、相同的仅打包扩展页面 CSP，以及相同的弹窗、图标、隔离内容脚本和 MAIN-world 观察器。Firefox for Android、签名和发布仍不在范围内，桌面最低版本不下调。

API 来源边界现在识别两种精确扩展方案。`is_browser_extension_origin` 接受 `chrome-extension://` 加 32 个 `a`–`p` 字符，或 `moz-extension://` 加小写 8-4-4-4-12 UUID。准入顺序、请求体与请求头限制、速率限制、类型化错误、内容类型检查、bearer 提取以及精确的令牌绑定来源均不变，也不添加通配 CORS 头。Firefox 令牌绑定到它自己的精确来源，因此把令牌用于另一扩展方案会被拒绝。

Firefox 接管在 `downloads.onCreated` 中评估，绝不在 `onDeterminingFilename` 中评估。适配器把 Firefox 下载映射到共享模型：把绝对路径缩减为经过净化的基本名并以 URL 推导名回退，把 `-1` 视为未知大小，并携带 referer、MIME 类型、`byExtensionId` 和隐身状态。共享控制器仍然先发送捕获，再取消并擦除浏览器下载，因此失败时浏览器文件保持完整，而先完成下载的快速场景会被如实报告为竞态而非被阻止。有界的接管失败记录在 Firefox 中保存在会话存储里，并在事件页启动时恢复，使弹窗重试路径在挂起后仍然存在；Chromium 把等价记录保存在内存中。两者都在成功接管或同 URL 预填充重试后清除该记录，且都不写入持久存储。资源元数据仍然仅限会话且有界。

构建从各自清单和各自来源替换写出 `extension/dist/chromium` 与 `extension/dist/firefox`，测试构建仍只写 Chromium 测试包。精确的 `web-ext` 依赖与 `npm run lint:extension:firefox` 校验构建出的 Firefox 包，前端 CI 任务在 Chromium 测试构建和扩展 Playwright 套件之前运行 Firefox 构建与 lint。不声称新的托管 CI 运行。

## Alternatives considered

- 在 Firefox 包内复制控制器被否决，因为接管、预填充、配对和资源边界的两份副本会漂移，并使需要测试的规则翻倍。
- 采用 WebExtension polyfill 被否决，因为所需表面很小，而本地类型化适配器能让依赖及其 promise 与回调差异保持明确。
- 为 Firefox 模拟 `onDeterminingFilename` 被否决，因为 Firefox 不提供等价的暂停点；使用 `downloads.onCreated` 并记录竞态才是诚实的边界。
- 接受任意 `moz-extension://` 字符串或不区分大小写的 UUID 被否决，因为那会扩大保护 bearer 令牌的可信来源边界。
- 把 Firefox 接管失败记录持久化到本地存储被否决，因为它是界面状态而非持久配对或设置数据，会话作用域既保护隐私又能承受它必须承受的挂起。
- 把浏览器绝对路径传给桌面端并依赖后续净化被否决，因为这会把本地路径泄漏进捕获协议并绕过文件名边界。
- 在本次变更中加入 Firefox for Android 支持、AMO 发布或签名被否决，因为该能力面向桌面 Firefox，发布路径属于独立工作。

## Consequences

共享控制器与适配器是两个包的唯一行为来源，Firefox 专属代码仅限于其清单、适配器、下载映射和事件安装。Chromium 保留其 service worker 监听器和 `onDeterminingFilename`，现有配对行、捕获行为和资源边界不变；Firefox 行添加自己的精确来源，不影响 Chromium 客户端。

捕获协议、配对 schema、任务存储 schema、设置 schema 和任务语义均不变。现有 Chromium 配对继续有效，Firefox 配对使用相同路由和限制，回滚 Firefox 包后 Chromium、任务、已完成文件和设置仍可使用。

安全性在种类上不变：没有新权限、Cookie、原生消息、远程代码或遥测，Firefox 清单声明不收集数据。来源判定、令牌绑定、准入顺序、限制和错误码是后续扩展工作必须保留的契约。`web-ext` lint 结果为 0 个错误和 1 条建议性警告，指出无数据收集声明在 Firefox for Android 上不受支持；由于 Android 不在范围内且桌面最低版本保持 140.0，该警告被接受。

自动化证据覆盖 Rust 来源与捕获套件、共享适配器与 Firefox 映射单元测试、两个生产构建、Chromium 测试构建、Firefox lint，以及现有前端、扩展和 Playwright 套件。尚未执行真实 Firefox 浏览器交互、事件页挂起、Windows Firefox 检查或手动 Chrome 与 Edge 记录，因此浏览器捕获与 Firefox 退出仍被阻塞。

- 上下文索引器只发现 TypeScript 模块；Rust 导航条目和生成特性条目的语义职责在重建后仍需手工维护。
- 自动化、mock 服务和 lint 结果绝不替代手动 Firefox 或 Windows 记录，待定人工项绝不被记录为通过。
- Firefox for Android、发布、签名、HLS 与 DASH 下载、视频解析、Cookie 访问和遥测仍不在本决策范围内。
