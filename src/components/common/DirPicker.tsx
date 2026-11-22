import { dialog } from "../../lib/tauri";
import { IconFile, IconFolder, IconFolderOpen } from "./icons";

/**
 * 通用「路径输入 + 浏览 + 可选打开位置」行内组件。
 *
 * 复用全站 `.path-field` 布局（输入框 flex:1 + 图标按钮 flex:none），
 * 按钮统一走 `.icon-btn`（34×34、描边、圆角 7px），与齿轮/其他字段旁图标按钮全局等大。
 *
 * 功能：
 * - 浏览按钮：弹系统原生文件/目录选择框（tauri_plugin_dialog），选中后调 onChange 回填。
 * - 打开按钮（onOpen 提供时出现）：在系统文件管理器中打开/定位该路径。
 * - 输入框可编辑（兼容 ~/... 快捷写法）；readOnly 模式用于不可编辑的展示路径。
 */
interface Props {
  value: string;
  onChange: (path: string) => void;
  placeholder?: string;
  /** 原生选择框标题。 */
  title?: string;
  /** "folder" 选目录（默认）；"file" 选文件。 */
  mode?: "folder" | "file";
  /** 浏览按钮 tooltip。 */
  browseLabel?: string;
  /** 输入框只读（如数据目录展示）。 */
  readOnly?: boolean;
  /** 提供后渲染"打开位置"图标按钮，点击调此回调。 */
  onOpen?: () => void;
  /** "打开位置"按钮 tooltip。 */
  openLabel?: string;
  /** 禁用浏览按钮（如迁移进行中）。 */
  disabled?: boolean;
}

export default function DirPicker({
  value,
  onChange,
  placeholder,
  title,
  mode = "folder",
  browseLabel,
  readOnly,
  onOpen,
  openLabel,
  disabled,
}: Props) {
  const browse = async () => {
    const p = mode === "folder" ? await dialog.pickFolder(title) : await dialog.pickFile(title);
    if (p) onChange(p);
  };
  return (
    <div className="path-field">
      <input
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        readOnly={readOnly}
        spellCheck={false}
        onFocus={readOnly ? (e) => e.target.select() : undefined}
      />
      {onOpen && (
        <button
          type="button"
          className="icon-btn"
          title={openLabel}
          onClick={onOpen}
        >
          <IconFolderOpen />
        </button>
      )}
      <button
        type="button"
        className="icon-btn"
        title={browseLabel}
        onClick={() => void browse()}
        disabled={disabled}
      >
        {mode === "folder" ? <IconFolder /> : <IconFile />}
      </button>
    </div>
  );
}
