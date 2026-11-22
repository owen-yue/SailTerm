// 聊天输入区的图片归一：把三个入口（粘贴 / 拖拽 / 📎 选文件）拿到的原始图，收敛成模型能收的形状。
//
// 为什么在前端压而不是原图直发：截图动辄 2560×1440、几 MB，而三条 provider 通道（OpenAI Chat
// Completions / Anthropic Messages / Ollama /api/chat）都是把 base64 塞进 JSON 请求体——原图既撞
// 体积上限也白烧 token。各家视觉模型的通用做法是先等比缩到最长边 ~1568px 再按有无透明通道选编码。
//
// 边界：本模块只管「进模型前」的形状，不管落盘归档（那是后端 `ai::attach` 的事）。
import type { ChatImage } from "../stores";
import type { StagedImage } from "../types";

/** 最长边阈值：主流视觉模型的安全上界，超出即等比缩放。 */
export const IMG_MAX_SIDE = 1568;
/** 无透明通道的重编码质量。 */
export const IMG_JPEG_QUALITY = 0.85;
/** 未缩放且原始字节不超过此值时原样透传（不重编码，保住截图的无损边缘）。 */
export const IMG_PASSTHROUGH_BYTES = 1024 * 1024;
/** 编码后单张上限；与后端 `attach::MAX_IMAGE_BYTES` 同值，前端先拦一层免得白跑一趟 IPC。 */
export const IMG_MAX_BYTES = 4 * 1024 * 1024;
/** 每轮张数上限；与后端 `attach::MAX_IMAGES_PER_TURN` 同值。 */
export const IMG_MAX_COUNT = 3;

/** 归一失败的原因；调用方据此出对应文案（把 i18n 留在组件侧，本模块不依赖语料）。 */
export type ImageReject = "tooMany" | "tooBig" | "unreadable";

/** `data:...;base64,...` 形式，可直接进 `<img src>`，也是发送时抽取 base64 的唯一留存。 */
function readAsDataUrl(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const fr = new FileReader();
    fr.onload = () => resolve(String(fr.result));
    fr.onerror = () => reject(new Error("read blob failed"));
    fr.readAsDataURL(blob);
  });
}

/** 画布上是否真含透明像素：透明图转 JPEG 会被填成白底，截图里的深色标注就没了，故先探一下。 */
function hasAlpha(ctx: CanvasRenderingContext2D, w: number, h: number): boolean {
  const { data } = ctx.getImageData(0, 0, w, h);
  for (let i = 3; i < data.length; i += 4) if (data[i] < 255) return true;
  return false;
}

/** 换编码就换扩展名，归档名与实际字节别对不上。 */
function rebase(name: string, type: string): string {
  const base = (name || "image").replace(/\.[^./\\]+$/, "") || "image";
  return `${base}${type === "image/png" ? ".png" : ".jpg"}`;
}

/**
 * 归一一张图：解码 → 需要时等比缩到最长边 ≤ 1568 → 重编码（小图且未缩放且本就在白名单里则原样透传）。
 * 返回 `ImageReject` 时不入列，由调用方 toast。
 */
export async function normalizeImage(file: File, have: number): Promise<ChatImage | ImageReject> {
  if (have >= IMG_MAX_COUNT) return "tooMany";
  let bmp: ImageBitmap;
  try {
    bmp = await createImageBitmap(file);
  } catch {
    // 扩展名像图但解不出来（损坏文件、或 webp/heic 之类浏览器不认的容器）
    return "unreadable";
  }
  const scale = Math.min(1, IMG_MAX_SIDE / Math.max(bmp.width, bmp.height));
  const inWhitelist = file.type === "image/png" || file.type === "image/jpeg";
  if (scale === 1 && inWhitelist && file.size <= IMG_PASSTHROUGH_BYTES) {
    bmp.close();
    return { name: file.name || "image", mime: file.type, dataUrl: await readAsDataUrl(file) };
  }
  const w = Math.max(1, Math.round(bmp.width * scale));
  const h = Math.max(1, Math.round(bmp.height * scale));
  const canvas = document.createElement("canvas");
  canvas.width = w;
  canvas.height = h;
  const ctx = canvas.getContext("2d");
  if (!ctx) {
    bmp.close();
    return "unreadable";
  }
  ctx.drawImage(bmp, 0, 0, w, h);
  bmp.close();
  // JPEG 天生不透明，不必回扫 alpha；其余容器（png/webp/gif/bmp）才需要判断有没有透明通道
  const type = file.type === "image/jpeg" || !hasAlpha(ctx, w, h) ? "image/jpeg" : "image/png";
  const blob = await new Promise<Blob | null>((res) => canvas.toBlob(res, type, IMG_JPEG_QUALITY));
  if (!blob) return "unreadable";
  if (blob.size > IMG_MAX_BYTES) return "tooBig";
  return { name: rebase(file.name, type), mime: type, dataUrl: await readAsDataUrl(blob) };
}

/** 📎 选文件入口：后端只负责把本地路径读成 base64（WebView 读不了任意本地字节），这里还原成 File 走同一条归一。 */
export function stagedToFile(s: StagedImage): File {
  const bin = atob(s.data);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return new File([bytes], s.name, { type: s.mime });
}
