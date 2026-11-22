// 剪贴板兼容封装：WebView 的 `navigator.clipboard` 在非安全上下文或权限受限时会直接抛错，
// 这里退回隐藏的 textarea + execCommand，保证「复制路径」在任何情况下都尽力可用。

/** 复制文本；返回是否成功（失败时由调用方给出提示）。 */
export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return legacyCopy(text);
  }
}

function legacyCopy(text: string): boolean {
  const ta = document.createElement("textarea");
  ta.value = text;
  // 移出视口但保持可选中：display:none 会让 execCommand 拿不到选区
  ta.style.position = "fixed";
  ta.style.top = "-1000px";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.select();
  let ok = false;
  try {
    ok = document.execCommand("copy");
  } catch {
    ok = false;
  }
  document.body.removeChild(ta);
  return ok;
}
