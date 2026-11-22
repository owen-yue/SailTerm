// 答疑直达：任意界面调 jumpToFaq(编号) → App 的监听器打开命令面板「帮助与答疑」页签，
// 展开并滚动到该条。编号 = faq.<lang>.md 语料标题里的显式 [N]（双语同编号 = 同一条答疑），
// 语料维护规则见 lib/faq 头注释；新增直达入口时把编号常量写在调用处并注明对应条目。
type FaqListener = (no: number) => void;

let listener: FaqListener | null = null;

/** App 挂载时注册监听（传入 null 注销）；只应有一个监听者。 */
export function setFaqListener(l: FaqListener | null): void {
  listener = l;
}

/** 打开帮助与答疑并直达指定编号的条目。 */
export function jumpToFaq(no: number): void {
  listener?.(no);
}
