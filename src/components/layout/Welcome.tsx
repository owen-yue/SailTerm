import logo from "../../assets/sailterm-logo.png";

/** 主区空态：只留品牌（logo + 名字）。动作入口在侧栏（新建连接）与顶栏（插件/设置等）。 */
export default function Welcome() {
  return (
    <div className="welcome">
      {/* 首页 logo 直接复用应用图标本体，与 Dock 图标完全一致 */}
      <img className="w-mark" src={logo} alt="SailTerm" />
      <h1>SailTerm</h1>
    </div>
  );
}