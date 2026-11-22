# 插件示例

安装：设置 → 插件 → 安装，选本目录（含 `manifest.json`）。

## manifest 能力一览

| 字段 | 作用 |
|------|------|
| `settings[]` | 插件设置表单（string/number/boolean/select） |
| `commands[]` | 命令包；`{{arg}}` 为右键传入的路径 / `{{key}}` 为设置值；`requires` 声明执行所需会话能力（见下） |
| `buttons[]` | 注入 UI 按钮（`target`: topbar / files / terminal） |
| `menus[]` | 右键菜单（`when`: file / dir / always） |
| `permissions` | `log` `notify` `storage` `session_exec` `network` `session_write` `bridge` `ai_tool` |
| `aiTools[]` | 给 AI 贡献技能（需同时声明 `ai_tool` 权限）：`name` 为模型可见工具名、`export` 为 wasm 入口、`risk` 静态定风险 |
| `hooks` | `on_output` `on_command` `on_timer` `on_connect` |
| `timerSecs` | `on_timer` 间隔（秒） |
| `bridges[]` | 字节桥名（X11 / 自定义协议扩展点） |
| `locales[]` | 为界面贡献新语言：`code`（语言码，非 `system`）/ `label`（展示名）/ `path`（插件目录内平面 JSON） |

## 命令能力档位（`commands[].requires`）

命令包统一经会话的 **exec 通道**执行，因此 `requires` 缺省为 `"exec"`。宿主按会话能力表
（`src/lib/capabilities.ts`，与后端 `SessionKind::caps()` 同源）门控：

- **FTP / FTPS 只有 `fs` + `negotiation`，没有 `exec`** —— 绑定了 `exec` 命令的右键菜单项
  （如 `SHA256 校验`、`尾部 15 行`）在这类会话上会**自动隐藏**，不再出现「点了必然失败」的入口；
  后端 `plugin_run_command` 也会二次拦截，给清晰引导而非底层报错。
- 本地文件栏（无关联会话）同理隐藏。
- 取值可为任一档位：`pty` / `exec` / `fs` / `monitor` / `negotiation` / `zmodem` / `tunnel`；
  填 `"none"` 表示该命令不依赖任何会话通路（宿主不再按能力门控）。

## 语言包（`locales[]`）

语言是**注册制**的：内置 `zh`/`en`，插件可经 `locales[]` 追加任意语言，与内置同权。

- `path` 指向插件目录内的 JSON，内容为平面「键 → 文案」（见 `locales/ja.json`）。
- 键名与内置语料一致（`settings.*` / `cat.*` / `general.*` / `term.*`）；缺键自动回退中文，再缺回退键名。
- 后端读取时限制在插件目录内（拒绝 `..` / 绝对路径），文件 ≤ 1MB，值必须全为字符串。
- 安装本示例后，设置 → 通用 → 界面语言会多出「日本語」；选中即整界面切换（终端输出不受影响）。

## WASM ABI（有 `wasm` 时）

- 导出：`plugin_alloc(len)->i32` · `memory` · `on_output(...)`
- 导入 `env`：`log` `notify` `get_setting` `set_setting` `session_write` `http_get` `bridge_send`

无 `wasm` 时只跑命令包模板（够用大多数「快捷命令」场景）。

## AI 技能（`aiTools[]`）

插件可把自有能力（查 CMDB / 内部工单 / 知识库等）作为**工具**交给 AI，而不只是往提示词里灌说明。模型在
会话里看到的就是一个普通 function，执行落回本插件 wasm。

- 必须同时在 `permissions` 声明 `ai_tool`（安装时可见，未声明则技能整体不下发）。
- 字段：`id`（包内唯一，供启用开关）/ `name`（模型可见工具名，限 `[a-z0-9_]{1,64}`）/ `description`（
  进 schema 的一句话描述：教模型何时用、边界在哪）/ `requires`（会话能力档位，缺省 `none`）/ `risk`
  （`readonly` 缺省直通，`confirm` 每次过审批卡）/ `export`（wasm 入口名，缺省 `ai_tool_<name>`）/
  `parameters`（OpenAI `function.parameters` 形态的 JSON Schema）。
- ABI：`export(p0,l0,p1,l1)->i64`，入参 `(tool_name, args_json)`，单 wasm 多技能时导出内部靠首参分派；
  回传字符串约定为 `{"ok":true,"text":...,"data":...}` 或 `{"error":...}` 的 JSON。
- 同名冲突：插件技能与内建工具同名时，**内建优先**、插件让位丢弃（记日志）。
- 启用开关：设置 → AI → 「AI 技能」列表可按技能停用（持久化为插件隔离设置 `skill_off:<id>`）。

本示例的 `cmdb_lookup` 为**只读**技能模板（`requires: none`，任意会话可下发），用户可照此结构自制。
