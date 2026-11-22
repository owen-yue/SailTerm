// vitest 全局 setup：只 mock「模块加载期就会触碰 Tauri 运行时」的边界底座。
// 被测对象都是纯逻辑模块——它们经 i18n 等间接 import `@tauri-apps/api`，
// jsdom 里没有 window.__TAURI_INTERNALS__，一旦有代码真去 invoke 就直接报错。
// 统一打桩成 no-op，测试里不需要逐个再 mock。
import { vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => undefined),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => undefined),
  emit: vi.fn(async () => undefined),
}));
