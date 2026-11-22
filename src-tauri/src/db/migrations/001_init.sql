-- SailTerm 初始 schema（v1）。
-- 原则：库内绝不出现明文凭证——密码 / 私钥 / API Key 一律以 *_ref 指向保险库（OS 钥匙串），
-- 发给 AI 的内容一律先经 Sanitizer 脱敏后才落库。
-- 时间戳均为 unix 毫秒（INTEGER NOT NULL）。

-- ── 连接档案 ────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS connection_profiles (
    id              TEXT PRIMARY KEY,            -- UUID
    name            TEXT NOT NULL,               -- 显示名/别名
    group_id        TEXT,                        -- 分组(可空)
    kind            TEXT NOT NULL DEFAULT 'ssh', -- ssh|sftp|ftp|ftps|telnet|serial|mosh
    host            TEXT NOT NULL,
    port            INTEGER NOT NULL DEFAULT 22,
    username        TEXT,
    auth_type       TEXT NOT NULL DEFAULT 'password', -- password|privateKey
    credential_ref  TEXT,                        -- 指向保险库(密码)
    private_key_ref TEXT,                        -- 指向保险库(私钥内容)
    startup_dir     TEXT,                        -- 登录后初始目录
    extra           TEXT,                        -- JSON: 私钥路径/keepalive 等非敏感扩展
    color_tag       TEXT,
    sort_order      INTEGER DEFAULT 0,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    private_key_id  TEXT                         -- 引用密钥库私钥(ssh_key.id)，优先于粘贴/路径
);
CREATE INDEX IF NOT EXISTS idx_conn_group ON connection_profiles(group_id);
CREATE INDEX IF NOT EXISTS idx_conn_name ON connection_profiles(name);

CREATE TABLE IF NOT EXISTS connection_groups (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    parent_id   TEXT,
    sort_order  INTEGER DEFAULT 0,
    created_at  INTEGER NOT NULL
);

-- ── 主机密钥（SSH known_hosts / FTPS 证书 TOFU）────────────────────
CREATE TABLE IF NOT EXISTS known_hosts (
    id            TEXT PRIMARY KEY,
    host_key_hash TEXT NOT NULL,                 -- 主机标识 host:port
    key_type      TEXT NOT NULL,                 -- ssh-rsa|ssh-ed25519|...
    key_blob      TEXT NOT NULL,                 -- base64 公钥
    fingerprint   TEXT NOT NULL DEFAULT '',      -- SHA256 指纹(展示用)
    created_at    INTEGER NOT NULL,
    UNIQUE(host_key_hash, key_type)
);

CREATE TABLE IF NOT EXISTS ftps_certs (
    id           TEXT PRIMARY KEY,
    ident        TEXT NOT NULL UNIQUE,  -- "host:port"：用户填写的控制地址（非 PASV 数据地址）
    fingerprint  TEXT NOT NULL,         -- SHA256:冒号十六进制，比对与展示同源
    subject      TEXT NOT NULL,         -- 证书主体 CN（展示用，可空串）
    validity     TEXT NOT NULL,         -- "2026-08-20 ~ 2027-08-20"（展示用）
    created_at   INTEGER NOT NULL
);

-- ── 命令历史（入库前经脱敏，不含内联密码）──────────────────────────
CREATE TABLE IF NOT EXISTS command_history (
    id           TEXT PRIMARY KEY,
    session_id   TEXT,
    profile_id   TEXT,                           -- 归属连接档案，历史行可为 NULL
    command      TEXT NOT NULL,
    source       TEXT NOT NULL DEFAULT 'user',   -- user|ai
    exit_code    INTEGER,
    created_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_cmd_profile ON command_history(profile_id, created_at);
CREATE INDEX IF NOT EXISTS idx_cmd_session ON command_history(session_id);

-- ── 应用设置（key-value，value 为 JSON 或标量字符串）────────────────
CREATE TABLE IF NOT EXISTS settings (
    key         TEXT PRIMARY KEY,
    value       TEXT NOT NULL,
    updated_at  INTEGER NOT NULL
);

-- ── 文件传输任务（只记录路径与进度，不含任何凭证；会话关闭后保留供续传与审计）──
CREATE TABLE IF NOT EXISTS transfer_tasks (
    id              TEXT PRIMARY KEY,
    session_id      TEXT NOT NULL,                    -- 归属会话（FTP/FTPS 亦复用）
    direction       TEXT NOT NULL CHECK (direction IN ('upload','download')),
    remote_path     TEXT NOT NULL,
    local_path      TEXT NOT NULL,
    total_bytes     INTEGER NOT NULL DEFAULT 0,
    done_bytes      INTEGER NOT NULL DEFAULT 0,       -- 已确认落盘字节数，续传起点
    status          TEXT NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending','running','done','failed','canceled')),
    error           TEXT,                             -- 失败原因（可读文案，非底层堆栈）
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    profile_id      TEXT,                             -- 归属连接档案
    phase           TEXT NOT NULL DEFAULT 'transfer', -- 打包传输阶段 compress|transfer|extract
    remote_cleaned  INTEGER NOT NULL DEFAULT 0        -- 崩溃回收：远端临时包是否已 best-effort 清理
);
CREATE INDEX IF NOT EXISTS idx_task_session ON transfer_tasks(session_id, status);
CREATE INDEX IF NOT EXISTS idx_task_created ON transfer_tasks(created_at);
CREATE INDEX IF NOT EXISTS idx_task_profile ON transfer_tasks(profile_id, created_at);

-- ── AI：provider 配置（API Key 只在保险库 key_ref，库内绝不出现明文）──
CREATE TABLE IF NOT EXISTS ai_providers (
    id                  TEXT PRIMARY KEY,
    name                TEXT NOT NULL,
    kind                TEXT NOT NULL
                        CHECK (kind IN ('openai','claude','ollama','openai-compatible')),
    base_url            TEXT,                        -- 兼容端点/自建/Ollama 地址；官方端点可空
    model               TEXT NOT NULL,
    key_ref             TEXT,                        -- 指向保险库；Ollama 本地可空
    supports_tools      INTEGER,                     -- 0/1/NULL(未探测)
    sanitize_level      TEXT NOT NULL DEFAULT 'strict'
                        CHECK (sanitize_level IN ('strict','standard','loose','custom')),
    max_context_tokens  INTEGER NOT NULL DEFAULT 8192,
    temperature         REAL,
    is_default          INTEGER NOT NULL DEFAULT 0,  -- 全局唯一默认（repo 层保证）
    extra               TEXT,                        -- JSON: 自定义 header/组织 id 等非敏感扩展
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL,
    supports_vision     INTEGER                      -- 1=支持图片 0=不支持 NULL=未探测
);

CREATE TABLE IF NOT EXISTS ai_conversations (
    id             TEXT PRIMARY KEY,
    connection_id  TEXT,                             -- 归档到哪个连接(profile_id)；空=通用对话
    title          TEXT,                             -- 对话标题(可自动生成/重命名)
    provider_id    TEXT,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_conv_conn ON ai_conversations(connection_id, updated_at);

-- 对话消息：存的是**脱敏后**的文本与工具调用记录，不含原始敏感输出
CREATE TABLE IF NOT EXISTS ai_chat_messages (
    id              TEXT PRIMARY KEY,
    conversation_id TEXT,                            -- 所属对话
    connection_id   TEXT,                            -- 冗余连接 id，便于按服务器直接过滤
    session_id      TEXT,                            -- 关联会话(可空：通用问答)
    provider_id     TEXT,
    role            TEXT NOT NULL
                    CHECK (role IN ('system','user','assistant','tool')),
    content         TEXT,                            -- 文本内容(脱敏后)
    tool_call_id    TEXT,                            -- role=tool 时对应的调用 id
    tool_name       TEXT,                            -- role=assistant 的工具调用名
    tool_args       TEXT,                            -- JSON，工具入参(不含凭证)
    tokens          INTEGER,
    created_at      INTEGER NOT NULL,
    steps           TEXT,                            -- 本轮过程轨迹 JSON（含思考链内联步骤；仅 assistant 行）
    context_used    INTEGER,                         -- 末次模型调用实际发送的上下文规模（仅 assistant 行）
    context_window  INTEGER,                         -- 该轮 provider 上下文窗口总量（仅 assistant 行）
    estimated       INTEGER,                         -- 1=provider 未回传用量(估算)；0=真值；NULL=未知
    attachments     TEXT                             -- JSON: [{"id","mime","name","bytes"}]，图片引用非字节，仅 user 行
);
CREATE INDEX IF NOT EXISTS idx_chat_conv ON ai_chat_messages(conversation_id, created_at);
CREATE INDEX IF NOT EXISTS idx_chat_session ON ai_chat_messages(session_id, created_at);

-- ── AI 安全闭环：审计与策略 ─────────────────────────────────────────
-- audit_log 只存**脱敏后**的提议/执行命令与输出摘要（哈希），绝不落原始敏感输出，更不存凭证。
CREATE TABLE IF NOT EXISTS audit_log (
    id                TEXT PRIMARY KEY,
    session_id        TEXT,                          -- 关联会话(可空：通用问答)
    actor             TEXT NOT NULL DEFAULT 'ai'
                      CHECK (actor IN ('ai','user')),
    proposed_command  TEXT,                          -- AI 提议命令(脱敏后)
    risk_level        TEXT
                      CHECK (risk_level IN ('READ_ONLY','WRITE','DANGEROUS','UNKNOWN')),
    decision          TEXT
                      CHECK (decision IN ('AUTO','APPROVED','DENIED','BLOCKED','EDITED','EXPIRED')),
    executed_command  TEXT,                          -- 实际执行(编辑改判后可能不同于提议)
    exit_code         INTEGER,
    output_digest     TEXT,                          -- 输出摘要/哈希，不存原始敏感输出
    ai_message_id     TEXT,                          -- 关联触发的 AI 消息(ai_chat_messages.id)
    created_at        INTEGER NOT NULL,
    profile_id        TEXT                           -- 归属连接档案（可空：通用问答）
);
CREATE INDEX IF NOT EXISTS idx_audit_session ON audit_log(session_id, created_at);
CREATE INDEX IF NOT EXISTS idx_audit_time ON audit_log(created_at);
CREATE INDEX IF NOT EXISTS idx_audit_profile ON audit_log(profile_id, created_at);

-- policy_rules：用户自定义黑/白/写名单，与内置规则合并参与分级；规则只是命令模式，本身不含凭证。
CREATE TABLE IF NOT EXISTS policy_rules (
    id          TEXT PRIMARY KEY,
    kind        TEXT NOT NULL
                CHECK (kind IN ('readonly_whitelist','write_list','blacklist')),
    pattern     TEXT NOT NULL,                       -- 命令/正则模式(由 PolicyEngine 解释)
    action      TEXT
                CHECK (action IN ('allow','approval','block')),  -- 空=按 kind 隐含动作
    note        TEXT,                                -- 规则说明(展示于审批弹窗)
    enabled     INTEGER NOT NULL DEFAULT 1,
    sort_order  INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_policy_kind ON policy_rules(kind, enabled);

-- ── AI 记忆：content 只存**脱敏后**的偏好/环境事实，绝不落凭证 ──────
CREATE TABLE IF NOT EXISTS memory (
    id             TEXT PRIMARY KEY,
    scope          TEXT NOT NULL DEFAULT 'server'
                   CHECK (scope IN ('global','server')),
    connection_id  TEXT,                             -- scope=server 时绑定; global 时为空
    content        TEXT NOT NULL,
    source         TEXT NOT NULL DEFAULT 'auto'
                   CHECK (source IN ('auto','manual')),
    keywords       TEXT,                             -- 召回关键词(可空, 逗号分隔)
    embedding      BLOB,                             -- 可选本地向量(不出网); 当前未启用
    created_at     INTEGER NOT NULL,
    last_used_at   INTEGER                           -- 最近命中时间, 用于淘汰
);
CREATE INDEX IF NOT EXISTS idx_mem_scope ON memory(scope, connection_id, last_used_at);

-- ── 命令片段：content 为命令模板，可含 {{变量}} 占位；命中凭证即拒存 ──
CREATE TABLE IF NOT EXISTS snippet (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,                      -- 片段名（列表展示）
    group_name   TEXT NOT NULL DEFAULT '',           -- 自由分组标签（空=未分组）
    content      TEXT NOT NULL,                      -- 命令模板（绝不存明文凭证）
    description  TEXT NOT NULL DEFAULT '',           -- 备注说明（可空）
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_snippet_group ON snippet(group_name, name);

-- ── SSH 密钥库：私钥材料只在保险库 key_ref，本表只留非敏感元数据 ────
CREATE TABLE IF NOT EXISTS ssh_key (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,                      -- 用户命名（列表展示）
    algorithm    TEXT NOT NULL,                      -- ssh-ed25519 / ssh-rsa / ecdsa-*（非敏感）
    fingerprint  TEXT NOT NULL,                      -- SHA256:... 指纹（非敏感，供核对）
    public_key   TEXT NOT NULL,                      -- authorized_keys 格式公钥串（公钥本就公开）
    comment      TEXT NOT NULL DEFAULT '',           -- 密钥自带注释（可空）
    encrypted    INTEGER NOT NULL DEFAULT 0,         -- 私钥是否带口令加密（0/1）
    key_ref      TEXT NOT NULL,                      -- 保险库键名（私钥绝不落本表）
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ssh_key_name ON ssh_key(name);
CREATE INDEX IF NOT EXISTS idx_ssh_key_fp ON ssh_key(fingerprint);

-- ── SSH 隧道 / 端口转发：转发规则（本地 ⇄ 远端 host:port 映射），绝不含凭证 ──
CREATE TABLE IF NOT EXISTS tunnel (
    id           TEXT PRIMARY KEY,
    profile_id   TEXT,                               -- 归属连接档案(松散引用)；空=不绑定
    name         TEXT NOT NULL,                      -- 用户命名（列表展示）
    kind         TEXT NOT NULL DEFAULT 'local',      -- local|remote|dynamic（当前仅 local 生效）
    local_host   TEXT NOT NULL DEFAULT '127.0.0.1',  -- 本地监听地址
    local_port   INTEGER NOT NULL,                   -- 本地监听端口（0=随机可用端口）
    remote_host  TEXT NOT NULL,                      -- 远端目标主机（经该 SSH 连接可达）
    remote_port  INTEGER NOT NULL,                   -- 远端目标端口
    enabled      INTEGER NOT NULL DEFAULT 1,         -- 是否启用（0/1）
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_tunnel_profile ON tunnel(profile_id);
CREATE INDEX IF NOT EXISTS idx_tunnel_name ON tunnel(name);

-- ── 输出触发器：对终端输出做正则匹配，触发告警 / 高亮 / 关键字着色；只存规则本身 ──
CREATE TABLE IF NOT EXISTS trigger_rule (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    pattern TEXT NOT NULL,
    action TEXT NOT NULL DEFAULT 'alert',            -- alert|highlight|colorize
    enabled INTEGER NOT NULL DEFAULT 1,
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    color TEXT NOT NULL DEFAULT 'red'                -- 语义色键 red|green|yellow|blue|magenta|cyan，真实色由主题解析
);

-- 内置默认着色规则：固定 id 便于幂等（INSERT OR IGNORE），用户可改名/停用/删除。
INSERT OR IGNORE INTO trigger_rule (id, name, pattern, action, color, enabled, sort_order, created_at, updated_at) VALUES
  ('sys-trg-error', 'error',
   '\b(error|errors|fatal|critical|fail(ed|s|ure)?|denied|refused|invalid|not found|no such|unreachable|seg(mentation)? fault|panic|traceback|exception|core dumped|killed|aborted|corrupt(ed|ion))\b',
   'colorize', 'red', 1, 10, 0, 0),
  ('sys-trg-warn', 'warning',
   '\b(warn(ing|ings)?|deprecated|unknown|unavailable|disabled|suspicious|retry(ing)?|timed out|timeout|cannot|unable to|out of (memory|space)|low (memory|disk))\b',
   'colorize', 'yellow', 1, 20, 0, 0),
  ('sys-trg-ok', 'success',
   '\b(success(ful(ly)?)?|ok|passed|accepted|allowed|enabled|connected|completed|verified|reachable|active|running|true|yes)\b',
   'colorize', 'green', 1, 30, 0, 0),
  ('sys-trg-info', 'info',
   '\b(info|notice|note|starting|started|loading|checking|created|listening|waiting|initialized|version)\b',
   'colorize', 'blue', 1, 40, 0, 0),
  ('sys-trg-net', 'ip / host',
   '\b((25[0-5]|2[0-4][0-9]|1?[0-9]?[0-9])\.){3}(25[0-5]|2[0-4][0-9]|1?[0-9]?[0-9])\b|\blocalhost\b',
   'colorize', 'magenta', 1, 50, 0, 0);

-- ── 插件设置：按 plugin_id 命名空间隔离；凭证不进此表（由能力网关托管进保险库）──
CREATE TABLE IF NOT EXISTS plugin_settings (
    plugin_id   TEXT NOT NULL,
    key         TEXT NOT NULL,
    value       TEXT NOT NULL DEFAULT '',
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (plugin_id, key)
);

-- ── 会话录制索引：录制文件散落各目录，起录时按绝对路径登记真实主机名；不含内容或凭证 ──
CREATE TABLE IF NOT EXISTS record_index (
    path        TEXT PRIMARY KEY,   -- 录制文件绝对路径
    host        TEXT NOT NULL,      -- 会话主机名（展示用）
    created_at  INTEGER NOT NULL
);
