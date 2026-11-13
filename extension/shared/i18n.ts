export type ExtensionLanguage = "zh-CN" | "en-US";

const EN_US = {
  "app.name": "OneGet",
  "popup.title": "OneGet downloads",
  "popup.resources": "Resources",
  "popup.resourcesEmpty": "No downloadable resources on this page.",
  "popup.resourcesCount": "{count} resources",
  "popup.resourceFromPage": "From {name}",
  "popup.unknownSize": "Size unknown",
  "popup.send": "Send to OneGet",
  "popup.captureSent": "Sent to OneGet.",
  "popup.captureFailed": "Could not send this resource to OneGet.",
  "popup.takeoverFailedHeading": "Browser download not taken over",
  "popup.takeoverRetry": "Send to OneGet again",
  "popup.takeoverFailed.capture_failed":
    "OneGet could not take over {filename}. Check that the app is running, then send it again.",
  "popup.takeoverFailed.token_revoked":
    "Pairing expired before {filename} could be taken over. Pair again, then retry.",
  "popup.takeoverFailed.update_required":
    "OneGet needs an update before it can take over {filename}.",
  "popup.manifestUnavailable": "HLS and DASH manifests cannot be downloaded directly.",
  "popup.kind.video": "Video",
  "popup.kind.audio": "Audio",
  "popup.kind.archive": "Archive",
  "popup.kind.program": "Program",
  "popup.kind.document": "Document",
  "popup.kind.model": "Model",
  "popup.kind.other": "Other",
  "popup.kind.hls": "HLS manifest",
  "popup.kind.dash": "DASH manifest",
  "popup.pairing": "Pairing",
  "popup.pairedWith": "Paired with {name}",
  "popup.unpaired": "Not paired",
  "popup.pairingCode": "Pairing code",
  "popup.pairingCodePlaceholder": "Eight-digit code",
  "popup.pair": "Pair",
  "popup.unpair": "Unpair",
  "popup.pairingFailed": "Pairing failed.",
  "popup.pairingRevoked": "Pairing is no longer valid. Pair again.",
  "popup.serviceUnavailable": "OneGet is not available. Start the app and try again.",
  "popup.updateRequired": "Update OneGet or the extension to continue.",
  "popup.settings": "Takeover settings",
  "popup.takeoverEnabled": "Take over browser downloads",
  "popup.minimumSize": "Minimum size in bytes",
  "popup.blacklist": "Blocked hosts",
  "popup.blacklistHint": "One host per line.",
  "popup.save": "Save",
  "popup.saved": "Settings saved.",
  "popup.invalidSettings": "Some settings values were invalid.",
  "overlay.download": "Download with OneGet",
  "overlay.unavailable": "Video download is not available yet.",
  "error.duplicate": "This download was already sent.",
  "error.generic": "Something went wrong.",
} as const;

export type ExtensionMessageKey = keyof typeof EN_US;

const ZH_CN = {
  "app.name": "OneGet",
  "popup.title": "OneGet 下载",
  "popup.resources": "资源",
  "popup.resourcesEmpty": "此页面没有可下载的资源。",
  "popup.resourcesCount": "{count} 个资源",
  "popup.resourceFromPage": "来自 {name}",
  "popup.unknownSize": "大小未知",
  "popup.send": "发送到 OneGet",
  "popup.captureSent": "已发送到 OneGet。",
  "popup.captureFailed": "无法将此资源发送到 OneGet。",
  "popup.takeoverFailedHeading": "浏览器下载未被接管",
  "popup.takeoverRetry": "重新发送到 OneGet",
  "popup.takeoverFailed.capture_failed":
    "OneGet 无法接管 {filename}。请确认应用正在运行，然后重试。",
  "popup.takeoverFailed.token_revoked":
    "在接管 {filename} 之前配对已失效。请重新配对后重试。",
  "popup.takeoverFailed.update_required":
    "OneGet 需要更新后才能接管 {filename}。",
  "popup.manifestUnavailable": "HLS 和 DASH 清单无法直接下载。",
  "popup.kind.video": "视频",
  "popup.kind.audio": "音频",
  "popup.kind.archive": "压缩包",
  "popup.kind.program": "程序",
  "popup.kind.document": "文档",
  "popup.kind.model": "模型",
  "popup.kind.other": "其他",
  "popup.kind.hls": "HLS 清单",
  "popup.kind.dash": "DASH 清单",
  "popup.pairing": "配对",
  "popup.pairedWith": "已与 {name} 配对",
  "popup.unpaired": "未配对",
  "popup.pairingCode": "配对码",
  "popup.pairingCodePlaceholder": "八位配对码",
  "popup.pair": "配对",
  "popup.unpair": "取消配对",
  "popup.pairingFailed": "配对失败。",
  "popup.pairingRevoked": "配对已失效，请重新配对。",
  "popup.serviceUnavailable": "OneGet 不可用，请启动应用后重试。",
  "popup.updateRequired": "请更新 OneGet 或扩展后继续。",
  "popup.settings": "接管设置",
  "popup.takeoverEnabled": "接管浏览器下载",
  "popup.minimumSize": "最小大小（字节）",
  "popup.blacklist": "屏蔽的主机",
  "popup.blacklistHint": "每行一个主机。",
  "popup.save": "保存",
  "popup.saved": "设置已保存。",
  "popup.invalidSettings": "部分设置值无效。",
  "overlay.download": "使用 OneGet 下载",
  "overlay.unavailable": "视频下载暂不可用。",
  "error.duplicate": "此下载已发送。",
  "error.generic": "出现错误。",
} satisfies Record<ExtensionMessageKey, string>;

export const EXTENSION_DICTIONARIES: Record<ExtensionLanguage, Record<string, string>> = {
  "zh-CN": ZH_CN,
  "en-US": EN_US,
};

export function resolveExtensionLanguage(value: string | null | undefined): ExtensionLanguage {
  if (typeof value !== "string") {
    return "en-US";
  }
  const normalized = value.trim().toLowerCase();
  if (normalized === "zh" || normalized.startsWith("zh-")) {
    return "zh-CN";
  }
  if (normalized === "en" || normalized.startsWith("en-")) {
    return "en-US";
  }
  return "en-US";
}

export function translateExtension(
  language: ExtensionLanguage,
  key: string,
  params?: Record<string, string | number>,
): string {
  const dictionary = EXTENSION_DICTIONARIES[language] as Record<string, string>;
  const template = dictionary[key] ?? key;
  return template.replace(/\{(\w+)\}/g, (placeholder, name: string) => {
    const value = params?.[name];
    return value === undefined ? placeholder : String(value);
  });
}
